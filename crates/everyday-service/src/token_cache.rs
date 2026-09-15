//! A per-account access token, kept fresh without ever holding a refresh
//! token itself.
//!
//! An access token is short-lived by design -- an hour for most providers --
//! and every IMAP connection, IDLE poll and SMTP send an account's sync task
//! makes wants one. Asking the provider for a fresh one before every single
//! request would mean a mailbox with a five-minute IDLE cadence spending
//! more round trips talking to the token endpoint than to the mailbox
//! itself; caching the access token in memory for as long as it is good is
//! what this exists for.
//!
//! # What this does *not* hold
//!
//! A refresh token. That is the durable secret -- the one that, if leaked,
//! lets somebody act as the account until it is revoked by hand -- and it
//! belongs in the vault's `SecretStore`, sealed, not sitting in this
//! process's heap for the life of the session. [`TokenCache::access_token`]
//! takes a `refresh_fn` closure instead: the caller already holds whatever
//! it needs to reach the provider (the account's `OAuthClient` and its
//! current refresh token, read out of the vault), and hands back the
//! [`Tokens`] that call produced. This cache reads only the access token and
//! its expiry out of that; the refresh token in it is the caller's business
//! going in *and* coming back out.
//!
//! # Rotation, surfaced through the same closure
//!
//! Some providers -- Google chief among them -- occasionally send back a
//! *new* refresh token alongside a routine access-token refresh, silently
//! retiring the old one. `Tokens::refresh_token` is `Some` exactly when this
//! response carried one. Because `refresh_fn` is the thing that called the
//! provider, it is also the thing already holding that answer -- so
//! persisting a rotated refresh token is the closure's own job, done before
//! it returns the `Tokens` this cache reads the access token out of, not a
//! second callback this type would otherwise need to invent. A `refresh_fn`
//! that ignores `refresh_token` when it is `Some` is a account that
//! eventually gets `invalid_grant` on an old token nobody meant to keep
//! using; see `docs/plans/mail.md`'s note on rotation under Phase 1.
//!
//! # One lock per account
//!
//! Two callers asking for the same account's token at once -- an IDLE
//! reconnect and a send racing each other -- must not both decide the token
//! is stale and both hit the provider, doubling the request and, on a
//! provider that rotates on every refresh, handing one of the two callers a
//! refresh token the other has already made stale. A `tokio::Mutex` per
//! account key serialises that: the second caller blocks until the first's
//! refresh has landed, then finds the cache already warm and never calls
//! `refresh_fn` at all.
//!
//! # The one exception to "never a refresh token": a rotation the vault
//! # refused
//!
//! The module doc above is still the rule for the refresh token a
//! `refresh_fn` was *handed* -- that one is the caller's business, going in
//! and out, and never touches this type. [`Self::set_pending_refresh_token`]
//! is a narrow exception for the refresh token a provider just *rotated to*,
//! when `crate::mailsync::credential::resolve` could not save it to the
//! vault (a locked disk, a write that raced something else): losing that
//! value outright would stand a real chance of stranding the account, since
//! the provider may already have retired the token the vault still has on
//! file. Held here, briefly, keyed by the same account key as the access
//! token cache, until a later refresh manages to persist it -- see
//! `credential::resolve`'s own docs for the retry this feeds into. Cleared
//! the moment it lands durably; never written to disk from here, never
//! logged.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use everyday_mail::oauth::{OAuthError, Tokens};
use tokio::sync::Mutex;

/// How long before an access token's own expiry [`TokenCache::access_token`]
/// treats it as already gone, rather than handing out a token that expires
/// mid-flight on a slow connection or a clock a few seconds fast.
const REFRESH_SKEW: Duration = Duration::from_secs(60);

struct Cached {
    access_token: String,
    /// A monotonic deadline, converted once from `Tokens::expires_at` on the
    /// way in -- not the `jiff::Timestamp` itself. Every later check against
    /// it goes through `tokio::time::Instant::now()`, which a test can move
    /// deterministically with `tokio::time::advance` (`jiff::Timestamp::now()`
    /// reads the real system clock and answers to nothing a test controls),
    /// and which cannot be pushed backwards by a wall-clock adjustment -- an
    /// NTP correction or a leap second -- the way a stored `jiff::Timestamp`
    /// compared against a fresh `jiff::Timestamp::now()` could be.
    deadline: tokio::time::Instant,
}

/// Convert an absolute expiry into a deadline measured from now, clamping a
/// token that (per the wall clock) has already expired to zero rather than
/// underflowing.
fn deadline_from(expires_at: jiff::Timestamp) -> tokio::time::Instant {
    let remaining = jiff::Timestamp::now().duration_until(expires_at);
    let remaining = if remaining.is_negative() { Duration::ZERO } else { remaining.unsigned_abs() };
    tokio::time::Instant::now() + remaining
}

/// Cached access tokens, keyed by whatever the caller uses to name an
/// account -- an `AccountId`'s string form, once that record exists; a
/// plain string today, since this crate does not depend on it. See the
/// module doc for what is and is not kept here.
#[derive(Default)]
pub struct TokenCache {
    entries: Mutex<HashMap<String, Arc<Mutex<Option<Cached>>>>>,
    /// See the module doc's "The one exception to 'never a refresh token'".
    /// A plain `std::sync::Mutex`, not the async `tokio::sync::Mutex`
    /// `entries` uses: every access here is a single map lookup or
    /// insert, never held across an `.await`, so the lighter lock is
    /// enough and never risks blocking an executor thread.
    pending_refresh: StdMutex<HashMap<String, String>>,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// A still-good access token for `account_key`, refreshing first if the
    /// cached one is missing or within [`REFRESH_SKEW`] of expiring.
    ///
    /// `refresh_fn` is called at most once per stale token, however many
    /// callers are waiting on this key at once -- see the module doc's note
    /// on the per-account lock.
    pub async fn access_token<F, Fut>(
        &self,
        account_key: &str,
        refresh_fn: F,
    ) -> Result<String, OAuthError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Tokens, OAuthError>>,
    {
        let slot = {
            let mut entries = self.entries.lock().await;
            entries
                .entry(account_key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        // Held for the rest of this call, refresh included -- this is the
        // serialisation the module doc describes. A second caller for the
        // same key parks here until the first is done, then reads the slot
        // this just filled instead of making its own request.
        let mut slot = slot.lock().await;

        let fresh_until = tokio::time::Instant::now() + REFRESH_SKEW;
        if let Some(cached) = slot.as_ref()
            && cached.deadline > fresh_until
        {
            return Ok(cached.access_token.clone());
        }

        let tokens = refresh_fn().await?;
        let access_token = tokens.access_token.clone();
        let deadline = deadline_from(tokens.expires_at);
        *slot = Some(Cached { access_token: access_token.clone(), deadline });
        Ok(access_token)
    }

    /// Drop whatever this cache knows about one account -- called when it is
    /// removed, or when a refresh comes back `invalid_grant` and the account
    /// needs to sign in again, so a stale access token is not handed out
    /// once more before that is noticed. Also drops any pending rotated
    /// refresh token this account still owed the vault a write for: either
    /// the account is gone, or signing in again is about to replace its
    /// credential outright, and neither leaves anything worth retrying a
    /// persist of.
    pub async fn forget(&self, account_key: &str) {
        self.entries.lock().await.remove(account_key);
        self.clear_pending_refresh_token(account_key);
    }

    /// A rotated refresh token an earlier
    /// `crate::mailsync::credential::resolve` minted but could not save to
    /// the vault, if one is still owed -- see the module doc.
    pub fn pending_refresh_token(&self, account_key: &str) -> Option<String> {
        self.pending_refresh.lock().unwrap().get(account_key).cloned()
    }

    /// Remember `token` as owed to the vault for `account_key`, overwriting
    /// whatever was owed before -- a second rotation before the first ever
    /// landed supersedes it outright, since the provider has moved on to
    /// the newer one regardless.
    pub fn set_pending_refresh_token(&self, account_key: &str, token: String) {
        self.pending_refresh.lock().unwrap().insert(account_key.to_string(), token);
    }

    /// `token` reached the vault -- nothing left owed.
    pub fn clear_pending_refresh_token(&self, account_key: &str) {
        self.pending_refresh.lock().unwrap().remove(account_key);
    }

    /// Drop every cached access token. Called when the vault locks: every
    /// account's sync task has already been stopped by then (see
    /// `Service::locked`), so nothing left in this cache can be used, and a
    /// bearer token is not worth keeping in memory for no purpose.
    pub async fn clear(&self) {
        self.entries.lock().await.clear();
    }

    /// [`Self::clear`], without an `async` this can be awaited: for
    /// `Service::close`, which closes a vault handle from ordinary
    /// synchronous code and cannot itself pause on a lock. Skips clearing,
    /// rather than blocking, on the rare chance a refresh is in flight at
    /// that exact moment -- `Service::locked` is the path that actually
    /// waits, and is what every real "give up the key" moment calls.
    pub fn try_clear(&self) {
        if let Ok(mut entries) = self.entries.try_lock() {
            entries.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn tokens(access_token: &str, ttl: Duration) -> Tokens {
        Tokens {
            access_token: access_token.to_string(),
            expires_at: jiff::Timestamp::now()
                .checked_add(jiff::SignedDuration::from_secs(ttl.as_secs() as i64))
                .unwrap(),
            refresh_token: None,
            scope: None,
        }
    }

    #[tokio::test]
    async fn refreshes_once_and_reuses_the_result() {
        let cache = TokenCache::new();
        let calls = Arc::new(AtomicU32::new(0));

        for _ in 0..3 {
            let calls = calls.clone();
            let got = cache
                .access_token("acct-1", || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(tokens("at-1", Duration::from_secs(3600)))
                    }
                })
                .await
                .unwrap();
            assert_eq!(got, "at-1");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a token still well within its lifetime is reused"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn refreshes_again_inside_the_skew_window() {
        let cache = TokenCache::new();
        let calls = Arc::new(AtomicU32::new(0));

        let refresh = |token: &'static str, calls: Arc<AtomicU32>| {
            cache_call(&cache, calls, token, Duration::from_secs(90))
        };
        assert_eq!(refresh("at-1", calls.clone()).await.unwrap(), "at-1");

        // 31 seconds left is inside the 60-second skew -- due for a refresh
        // even though the token has not technically expired yet.
        tokio::time::advance(Duration::from_secs(59)).await;
        assert_eq!(refresh("at-2", calls.clone()).await.unwrap(), "at-2");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    async fn cache_call(
        cache: &TokenCache,
        calls: Arc<AtomicU32>,
        token: &'static str,
        ttl: Duration,
    ) -> Result<String, OAuthError> {
        cache
            .access_token("acct-1", move || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(tokens(token, ttl))
                }
            })
            .await
    }

    #[tokio::test]
    async fn two_callers_for_the_same_account_only_refresh_once() {
        let cache = Arc::new(TokenCache::new());
        let calls = Arc::new(AtomicU32::new(0));

        let mut set = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let calls = calls.clone();
            set.spawn(async move {
                cache
                    .access_token("acct-shared", || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            // Give the other seven callers a chance to reach
                            // the same lock while this one is "in flight" --
                            // if the lock did not serialise them, more than
                            // one would pass through here.
                            tokio::task::yield_now().await;
                            Ok(tokens("at-shared", Duration::from_secs(3600)))
                        }
                    })
                    .await
                    .unwrap()
            });
        }
        while let Some(result) = set.join_next().await {
            assert_eq!(result.unwrap(), "at-shared");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn different_accounts_do_not_share_a_token() {
        let cache = TokenCache::new();
        let a = cache
            .access_token("acct-a", || async { Ok(tokens("at-a", Duration::from_secs(3600))) })
            .await;
        let b = cache
            .access_token("acct-b", || async { Ok(tokens("at-b", Duration::from_secs(3600))) })
            .await;
        assert_eq!(a.unwrap(), "at-a");
        assert_eq!(b.unwrap(), "at-b");
    }

    #[tokio::test]
    async fn a_rotated_refresh_token_is_the_closures_own_business() {
        // `TokenCache` never sees a refresh token at all -- this proves it
        // by having the closure be the only thing that records rotation,
        // exactly as `everyday-service`'s real caller (reading and writing
        // the vault's `SecretStore`) will.
        let cache = TokenCache::new();
        let persisted_refresh_token = Arc::new(std::sync::Mutex::new(None::<String>));
        let store = persisted_refresh_token.clone();

        let got = cache
            .access_token("acct-1", move || {
                let store = store.clone();
                async move {
                    let mut tokens = tokens("at-1", Duration::from_secs(3600));
                    tokens.refresh_token = Some("rt-rotated".to_string());
                    *store.lock().unwrap() = tokens.refresh_token.clone();
                    Ok(tokens)
                }
            })
            .await
            .unwrap();

        assert_eq!(got, "at-1");
        assert_eq!(persisted_refresh_token.lock().unwrap().as_deref(), Some("rt-rotated"));
    }

    #[tokio::test]
    async fn a_refresh_failure_is_not_cached() {
        let cache = TokenCache::new();
        let calls = Arc::new(AtomicU32::new(0));

        let err = cache
            .access_token("acct-1", || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(OAuthError::InvalidGrant { description: "revoked".to_string() })
                }
            })
            .await
            .unwrap_err();
        assert!(matches!(err, OAuthError::InvalidGrant { .. }));

        // A second call tries again rather than replaying the failure --
        // there is nothing cached to replay.
        let got = cache
            .access_token("acct-1", || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(tokens("at-1", Duration::from_secs(3600)))
                }
            })
            .await
            .unwrap();
        assert_eq!(got, "at-1");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn forget_clears_the_cached_token() {
        let cache = TokenCache::new();
        let calls = Arc::new(AtomicU32::new(0));
        let call = |calls: Arc<AtomicU32>| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(tokens("at-1", Duration::from_secs(3600)))
            }
        };
        cache.access_token("acct-1", || call(calls.clone())).await.unwrap();
        cache.forget("acct-1").await;
        cache.access_token("acct-1", || call(calls.clone())).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

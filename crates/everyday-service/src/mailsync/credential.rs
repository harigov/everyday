//! Turning an account's [`AuthMethod`] and stored secret into a
//! [`Credential`] a [`MailSession`](everyday_mail::session::MailSession) can
//! authenticate with.
//!
//! One function, [`resolve`], and one job it has to get right: telling a
//! credential that is merely *unavailable right now* (the network is down,
//! the token endpoint is having a bad afternoon) apart from one that is
//! *wrong* (a revoked refresh token, a changed password) -- because
//! `docs/plans/mail.md` asks for two different responses. The first is
//! retried with the supervisor's backoff; the second sets
//! [`AccountStatus::NeedsSignIn`] and ends the task without another attempt,
//! since retrying a wrong credential only trains a server to rate-limit the
//! account.
//!
//! # A rotated refresh token the vault refused to write
//!
//! Some providers (Google chief among them) send back a fresh refresh
//! token alongside a routine access-token refresh, silently retiring the
//! old one -- and the vault write that saves it is an ordinary fallible
//! write like any other, which used to mean a locked disk or a write racing
//! something else could simply lose the new token, stranding the account on
//! a refresh token the provider had already thrown away. `resolve` now
//! retries that write once, and if it still fails, hands the token to
//! [`crate::token_cache::TokenCache`] rather than dropping it -- see that
//! type's own module doc for exactly what it is and is not trusted with --
//! so the very next refresh both authenticates with the value that actually
//! works and tries the write again.

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::{Account, AccountStatus, AuthMethod};
use everyday_mail::oauth::{OAuthClient, OAuthError};
use everyday_mail::session::Credential;
use jiff::Timestamp;

use crate::error::CommandResult;
use crate::service::{Service, blocking};

/// What [`resolve`] answers with.
pub enum Resolved {
    Ready(Credential),
    /// The credential is gone or refused. The caller should set
    /// [`AccountStatus::NeedsSignIn`] with this reason and stop -- see the
    /// module docs.
    NeedsSignIn(String),
    /// The provider could not be reached, or answered in a way that is not
    /// this credential's fault. Worth retrying with backoff.
    Transient(String),
}

/// Resolve `account`'s credential, refreshing an OAuth access token through
/// `svc`'s [`crate::token_cache::TokenCache`] and persisting a rotated
/// refresh token as it goes -- see that type's own module doc on why the
/// refresh token itself never passes through the cache.
pub async fn resolve(svc: &Arc<Service>, vault: &Arc<Vault>, account: &Account) -> Resolved {
    let secret = match vault.account_secret(account.id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Resolved::NeedsSignIn("this account has never been signed in to".into());
        }
        Err(e) => return Resolved::Transient(e.to_string()),
    };

    match &account.auth {
        AuthMethod::Password { username } => match secret.password {
            Some(password) => Resolved::Ready(Credential::Password {
                user: username.clone(),
                pass: password.into(),
            }),
            None => Resolved::NeedsSignIn("no password is stored for this account".into()),
        },
        AuthMethod::OAuth { client_id, auth_url, token_url, scopes } => {
            let Some(mut refresh_token) = secret.refresh_token.clone() else {
                return Resolved::NeedsSignIn("this account has never signed in with OAuth".into());
            };
            let account_id = account.id;
            let account_key = account_id.to_string();
            // A rotation from an earlier refresh that could not be saved to
            // the vault takes precedence over whatever the vault still
            // says: the provider may already have retired that one, and
            // this is what lets this call succeed with the one it actually
            // accepted last time -- see `TokenCache`'s own module doc.
            if let Some(pending) = svc.token_cache().pending_refresh_token(&account_key) {
                refresh_token = pending;
            }
            let client = OAuthClient {
                client_id: client_id.clone(),
                client_secret: secret.client_secret.clone(),
                auth_url: auth_url.clone(),
                token_url: token_url.clone(),
                scopes: scopes.clone(),
                // Only ever used for `begin`, which this path never calls --
                // `refresh` needs no redirect URI at all.
                redirect: String::new(),
            };

            let vault_for_rotation = vault.clone();
            let svc_for_rotation = svc.clone();
            let refresh_token_for_call = refresh_token.clone();
            let result = svc
                .token_cache()
                .access_token(&account_key, move || {
                    let vault = vault_for_rotation.clone();
                    let svc = svc_for_rotation.clone();
                    let client = client.clone();
                    let refresh_token = refresh_token_for_call.clone();
                    async move {
                        let tokens = client.refresh(&refresh_token).await?;
                        // What this refresh owes the vault: the token the
                        // provider just rotated to, when it did -- or, when
                        // it did not, the one this call actually
                        // authenticated with, *if* an earlier attempt at
                        // saving that same value never landed. Either way
                        // this is the one place that knows whether a write
                        // is owed at all; see `Tokens::refresh_token`'s own
                        // docs and `TokenCache`'s module doc on why the
                        // cache itself never sees the token this closure
                        // was called with, only ever one it is about to
                        // persist.
                        let key = account_id.to_string();
                        let owed = tokens.refresh_token.clone().or_else(|| {
                            svc.token_cache()
                                .pending_refresh_token(&key)
                                .map(|_| refresh_token.clone())
                        });
                        if let Some(new_refresh_token) = owed {
                            persist_rotated_refresh_token_resilient(
                                &svc,
                                &vault,
                                account_id,
                                new_refresh_token,
                                &tokens,
                            )
                            .await;
                        }
                        Ok(tokens)
                    }
                })
                .await;

            match result {
                Ok(access_token) => Resolved::Ready(Credential::XOAuth2 {
                    user: account.address.clone(),
                    access_token: access_token.into(),
                }),
                Err(OAuthError::InvalidGrant { description }) => Resolved::NeedsSignIn(description),
                Err(OAuthError::InvalidClient { description }) => {
                    Resolved::NeedsSignIn(description)
                }
                Err(OAuthError::Transient { message }) => Resolved::Transient(message),
                Err(OAuthError::Provider { code, description }) => {
                    Resolved::Transient(format!("{code}: {description}"))
                }
            }
        }
    }
}

/// Save a rotated refresh token, tolerating the vault write failing --
/// retried once immediately, and if that also fails, kept in
/// [`crate::token_cache::TokenCache`] (never logged, never dropped) so the
/// very next refresh both uses it and tries the write again, per that
/// type's own module doc. `spawn_blocking`'d (through [`blocking`]) rather
/// than run inline: this closure runs on the async executor, and a vault
/// write is not.
async fn persist_rotated_refresh_token_resilient(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account_id: everyday_core::id::AccountId,
    new_refresh_token: String,
    tokens: &everyday_mail::oauth::Tokens,
) {
    let access_token = tokens.access_token.clone();
    let expires_at = tokens.expires_at;
    let saved = persist_with_retry_and_pending_cache(
        &svc.token_cache(),
        &account_id.to_string(),
        new_refresh_token.clone(),
        move || {
            let vault = vault.clone();
            let refresh_token = new_refresh_token.clone();
            let access_token = access_token.clone();
            blocking(move || {
                Ok(persist_rotated_refresh_token(
                    &vault,
                    account_id,
                    &refresh_token,
                    &access_token,
                    expires_at,
                )?)
            })
        },
    )
    .await;

    if !saved {
        // No token in this line, per house style -- the point of keeping it
        // in `TokenCache` at all is that this failure is not the last word.
        tracing::warn!(
            account_id = %account_id,
            "could not save a rotated refresh token after retrying; keeping it in memory and trying again on the next refresh"
        );
    }
}

/// The retry-once-then-keep-in-memory algorithm [`persist_rotated_refresh_token_resilient`]
/// wraps a real vault write in, pulled out generic over `persist` so a test
/// can drive it against a fake write that fails a chosen number of times,
/// with no vault, no OAuth endpoint, and no `spawn_blocking` anywhere in
/// reach. Always remembers `value` in `cache` first (so a refresh that
/// fires again before this settles still sees the freshest token), and
/// clears it again only once `persist` has actually succeeded. Returns
/// whether it did.
async fn persist_with_retry_and_pending_cache<F, Fut>(
    cache: &crate::token_cache::TokenCache,
    key: &str,
    value: String,
    mut persist: F,
) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = CommandResult<()>>,
{
    cache.set_pending_refresh_token(key, value);
    let mut saved = false;
    for _attempt in 0..2 {
        if persist().await.is_ok() {
            saved = true;
            break;
        }
    }
    if saved {
        cache.clear_pending_refresh_token(key);
    }
    saved
}

fn persist_rotated_refresh_token(
    vault: &Vault,
    account_id: everyday_core::id::AccountId,
    refresh_token: &str,
    access_token: &str,
    expires_at: Timestamp,
) -> everyday_core::Result<()> {
    let mut secret = vault.account_secret(account_id)?.unwrap_or_default();
    secret.refresh_token = Some(refresh_token.to_string());
    secret.access_token = Some((access_token.to_string(), expires_at));
    vault.save_account_secret(account_id, &secret)
}

/// Move `account` to [`AccountStatus::NeedsSignIn`] with `reason`, if it is
/// not there already with the same words -- an idempotent write, since this
/// is called from a loop that may see the same failure more than once before
/// something stops asking.
pub fn mark_needs_sign_in(vault: &Vault, account: &Account, reason: &str) {
    if matches!(&account.status, AccountStatus::NeedsSignIn { reason: r } if r == reason) {
        return;
    }
    let mut account = account.clone();
    account.status = AccountStatus::NeedsSignIn { reason: reason.to_string() };
    account.updated_at = Timestamp::now();
    let _ = vault.save_account(&account);
}

/// Move `account` to [`AccountStatus::Error`] with `message` -- a server
/// that refused or could not be reached for a reason signing in again will
/// not fix. Named after the calendar's own rule: "a feed that is down is a
/// state, not a dialog."
pub fn mark_error(vault: &Vault, account: &Account, message: &str) {
    if matches!(&account.status, AccountStatus::Error { message: m } if m == message) {
        return;
    }
    let mut account = account.clone();
    account.status = AccountStatus::Error { message: message.to_string() };
    account.updated_at = Timestamp::now();
    let _ = vault.save_account(&account);
}

/// Move `account` back to [`AccountStatus::Ok`] and stamp `last_synced_at`.
pub fn mark_ok(vault: &Vault, account: &Account) {
    let mut account = account.clone();
    account.status = AccountStatus::Ok;
    account.last_synced_at = Some(Timestamp::now());
    account.updated_at = Timestamp::now();
    let _ = vault.save_account(&account);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CommandError;
    use crate::token_cache::TokenCache;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// The regression for the bug this module's "A rotated refresh token
    /// the vault refused to write" section describes: a write that fails
    /// once must not lose the token -- it is kept in `TokenCache` and the
    /// retry inside the same call picks it up.
    #[tokio::test]
    async fn a_write_that_fails_once_still_saves_on_the_immediate_retry() {
        let cache = TokenCache::new();
        let calls = AtomicU32::new(0);

        let saved = persist_with_retry_and_pending_cache(&cache, "acct-1", "rt-new".into(), || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(CommandError::new(crate::error::codes::IO, "write failed"))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert!(saved, "the immediate retry must have saved it");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            cache.pending_refresh_token("acct-1"),
            None,
            "a token that landed durably is no longer owed"
        );
    }

    /// Two failures in a row -- the write is not durable yet, but the token
    /// is not lost: it stays in `TokenCache`, ready for
    /// `crate::mailsync::credential::resolve`'s own next refresh to try
    /// persisting again, per the module doc.
    #[tokio::test]
    async fn a_write_that_keeps_failing_keeps_the_token_in_memory() {
        let cache = TokenCache::new();
        let calls = AtomicU32::new(0);

        let saved = persist_with_retry_and_pending_cache(&cache, "acct-1", "rt-new".into(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err(CommandError::new(crate::error::codes::IO, "write failed")) }
        })
        .await;

        assert!(!saved);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "retried exactly once, not looped forever");
        assert_eq!(
            cache.pending_refresh_token("acct-1"),
            Some("rt-new".to_string()),
            "the token must not be lost while nothing has saved it yet"
        );
    }

    #[tokio::test]
    async fn a_second_rotation_before_the_first_lands_supersedes_it() {
        let cache = TokenCache::new();
        cache.set_pending_refresh_token("acct-1", "rt-first".into());

        let saved =
            persist_with_retry_and_pending_cache(&cache, "acct-1", "rt-second".into(), || async {
                Err(CommandError::new(crate::error::codes::IO, "still down"))
            })
            .await;

        assert!(!saved);
        assert_eq!(
            cache.pending_refresh_token("acct-1"),
            Some("rt-second".to_string()),
            "the provider has moved on to the newer token regardless of what this process saved"
        );
    }
}

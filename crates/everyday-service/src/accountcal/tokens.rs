//! Turning an account's stored credential into something `accountcal` can
//! put on an HTTP request, without the refresh token or the password ever
//! leaving this module.
//!
//! # Two credentials, one entry point
//!
//! An account signs in one of two ways -- see [`AuthMethod`] -- and a
//! calendar sync needs whichever one it has: a CalDAV request against
//! iCloud or Fastmail wants HTTP Basic with the app password; one against
//! Google's CalDAV endpoint, the Google Calendar API, or Microsoft Graph
//! wants a Bearer access token. [`credential`] is the one function every
//! adapter calls, and it hides that branch behind [`Credential`] so
//! `caldav.rs`, `google.rs` and `graph.rs` never touch [`AuthMethod`] or
//! [`everyday_core::account::AccountSecret`] directly.
//!
//! # Extending `TokenCache` to key by resource
//!
//! `TokenCache::access_token` (`crate::token_cache`) already takes an
//! arbitrary string as its cache key -- it was never specific to "one
//! account, one token". Phase 6 needs more than one token per account for
//! exactly one provider: a Microsoft account's `common`-tenant refresh
//! token is not scoped to a single resource the way its *access* tokens
//! are, and mail's own sync task already holds one access token for
//! `outlook.office.com` (IMAP/SMTP) cached under that account's plain id.
//! Asking for a Graph token under the *same* key would either hand the
//! calendar sync a token that cannot call Graph, or silently evict the
//! token mail is mid-flight with -- whichever request happened to refresh
//! last would win, and the other would fail with a confusing 401 until its
//! own next refresh.
//!
//! The fix needs no change to `TokenCache` at all: [`Resource::cache_key`]
//! composes `"{account_id}:graph"` for Graph and leaves mail's own plain
//! `account_id` key alone for everything else (IMAP/SMTP, and CalDAV, which
//! for Google reuses the *same* access token IMAP does -- Google issues one
//! token good for every scope a single consent granted, unlike Microsoft's
//! per-resource split). That is the whole of "extending `TokenCache` to key
//! by `(account, resource_scope)`": a naming convention on the string the
//! cache was always going to take, documented here so the next resource
//! this application adds a token for follows the same shape rather than
//! inventing its own.
//!
//! # What happens to the token afterwards
//!
//! Nothing from this module is ever logged, and neither is the credential
//! [`Credential::Basic`] carries -- see each type's `Debug` impl, and the
//! house rule at the top of `docs/plans/mail.md` that a token must never
//! appear in a log or an error. A refresh that comes back `invalid_grant`
//! moves the account to [`AccountStatus::NeedsSignIn`] and forgets the
//! cached token for every resource of that account, which is the same
//! signal mail's own sync gives -- see [`mark_needs_sign_in`].

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::{Account, AccountStatus, AuthMethod};
use everyday_core::id::AccountId;
use everyday_mail::oauth::{OAuthClient, OAuthError};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

/// Which resource an OAuth access token is good for. See the module doc's
/// "Extending `TokenCache`" section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// Whatever `AuthMethod::OAuth::scopes` already names: IMAP/SMTP for
    /// mail, and -- for Google -- its CalDAV endpoint too, since Google
    /// issues one token good for every scope a single consent granted.
    Native,
    /// Microsoft Graph, asked for with its own `.default` scope over the
    /// same refresh token -- see [`OAuthClient::refresh_for_scopes`].
    Graph,
}

impl Resource {
    fn cache_key(self, account_id: AccountId) -> String {
        match self {
            Resource::Native => account_id.to_string(),
            Resource::Graph => format!("{account_id}:graph"),
        }
    }

    /// The scope requested on a refresh for this resource. `Native` sends
    /// none at all -- see [`OAuthClient::refresh`]'s doc for why that
    /// already means "whatever was granted before" -- so this is only
    /// consulted for `Graph`.
    fn graph_scopes() -> Vec<String> {
        vec!["https://graph.microsoft.com/.default".to_string(), "offline_access".to_string()]
    }
}

/// HTTP Basic credentials for a CalDAV request against a password-only
/// provider. Never printed: see the hand-written [`std::fmt::Debug`] below.
pub struct BasicAuth {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for BasicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicAuth")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

/// What [`credential`] hands back: either half of [`AuthMethod`], reduced to
/// exactly what an HTTP request needs.
pub enum Credential {
    Bearer(String),
    Basic(BasicAuth),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Bearer(_) => write!(f, "Credential::Bearer([redacted])"),
            Credential::Basic(b) => write!(f, "Credential::Basic({b:?})"),
        }
    }
}

/// The credential to authenticate a request to `resource` with, for
/// `account`. Refreshes an OAuth access token first if the cached one is
/// missing or about to expire; see [`Service::token_cache`].
pub async fn credential(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    resource: Resource,
) -> CommandResult<Credential> {
    match &account.auth {
        AuthMethod::Password { username } => {
            let account_id = account.id;
            let vault = vault.clone();
            let secret = blocking(move || Ok(vault.account_secret(account_id)?)).await?;
            let password = secret.and_then(|s| s.password).ok_or_else(|| {
                CommandError::new(
                    codes::FORBIDDEN,
                    "this account has no password saved; sign in again to add one",
                )
            })?;
            Ok(Credential::Basic(BasicAuth { username: username.clone(), password }))
        }
        AuthMethod::OAuth { client_id, auth_url, token_url, scopes } => {
            let token = access_token(
                svc,
                vault,
                account,
                resource,
                client_id.clone(),
                auth_url.clone(),
                token_url.clone(),
                scopes.clone(),
            )
            .await?;
            Ok(Credential::Bearer(token))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn access_token(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    resource: Resource,
    client_id: String,
    auth_url: String,
    token_url: String,
    native_scopes: Vec<String>,
) -> CommandResult<String> {
    let account_id = account.id;
    let cache = svc.token_cache();
    let key = resource.cache_key(account_id);

    // The client secret, when this provider issued one, lives beside the
    // refresh token in `AccountSecret` -- read once, up front, rather than
    // inside the cache's closure, because it does not change between
    // attempts the way the refresh token can.
    let vault_for_secret = vault.clone();
    let stored = blocking(move || Ok(vault_for_secret.account_secret(account_id)?)).await?;
    let Some(stored) = stored else {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "this account has never signed in; sign in first to read its calendars",
        ));
    };
    let Some(refresh_token) = stored.refresh_token.clone() else {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "this account has no refresh token saved; sign in again",
        ));
    };

    let oauth = OAuthClient {
        client_id,
        client_secret: stored.client_secret.clone(),
        auth_url,
        token_url,
        scopes: native_scopes,
        // Not used for *content* -- refreshing a token sends no redirect_uri
        // at all, only `begin` and `exchange` do -- but it still has to be a
        // syntactically valid URL: `OAuthClient::configured` builds
        // `RedirectUrl::new` unconditionally for all three verbs, sharing
        // one setup step, and an empty string fails that parse (the `url`
        // crate's own "relative URL without a base") before `refresh` ever
        // gets to send a request. An arbitrary, unreachable address, never
        // read past this parse.
        redirect: "http://localhost/unused-on-refresh".to_string(),
    };
    let graph_scopes = Resource::graph_scopes();
    let vault_for_save = vault.clone();

    let result = cache
        .access_token(&key, move || async move {
            let tokens = match resource {
                Resource::Native => oauth.refresh(&refresh_token).await?,
                Resource::Graph => oauth.refresh_for_scopes(&refresh_token, &graph_scopes).await?,
            };
            // Persist what changed. A rotated refresh token has to be saved
            // regardless of which resource asked for it -- there is only
            // one refresh token per account, and losing a rotation here
            // because it happened on a calendar sync rather than a mail one
            // would eventually turn into `invalid_grant` on both. The
            // *access* token is cached in `AccountSecret` only for the
            // native resource, mirroring what `attach_oauth_sign_in`
            // already stores there; a Graph token is short-lived, per
            // account, per process, and not worth a second column.
            if tokens.refresh_token.is_some() || matches!(resource, Resource::Native) {
                let tokens_for_save = tokens.clone();
                let outcome = blocking(move || {
                    let mut secret =
                        vault_for_save.account_secret(account_id)?.unwrap_or_default();
                    if let Some(rt) = tokens_for_save.refresh_token {
                        secret.refresh_token = Some(rt);
                    }
                    if matches!(resource, Resource::Native) {
                        secret.access_token =
                            Some((tokens_for_save.access_token.clone(), tokens_for_save.expires_at));
                    }
                    Ok(vault_for_save.save_account_secret(account_id, &secret)?)
                })
                .await;
                // A failure to *persist* the rotation must not fail the
                // sync that is already holding a good token in hand -- it
                // is logged and the token is used anyway; the next refresh
                // tries the save again.
                if let Err(e) = outcome {
                    tracing::warn!(%account_id, error = %e, "could not save a refreshed calendar token");
                }
            }
            Ok(tokens)
        })
        .await;

    match result {
        Ok(access_token) => Ok(access_token),
        Err(OAuthError::InvalidGrant { description }) => {
            cache.forget(&key).await;
            mark_needs_sign_in(vault, account_id, &description).await;
            Err(CommandError::new(
                codes::FORBIDDEN,
                "this account needs to sign in again before its calendars can be read",
            ))
        }
        Err(OAuthError::InvalidClient { .. }) => {
            Err(CommandError::new(codes::FORBIDDEN, "the account's client id or secret is wrong"))
        }
        Err(e) => Err(CommandError::new(codes::NETWORK, e.to_string())),
    }
}

/// Move an account to `NeedsSignIn`, the same state mail's own token refresh
/// moves it to on `invalid_grant`. `why` is the provider's own words, which
/// carry no token, so it is safe to keep -- see [`OAuthError::InvalidGrant`].
///
/// Called from two places. [`access_token`], below, calls it the moment an
/// OAuth refresh itself comes back `invalid_grant` -- the earliest possible
/// point, before any request that needed the token was even attempted.
/// `super::note_if_credential_is_bad` calls it again, after the fact, for
/// the credential this module cannot validate up front at all: a CalDAV app
/// password is never exchanged with a token endpoint, so a `FORBIDDEN`
/// result from the server itself, on the actual request, is the first this
/// application learns that one was revoked. `pub(crate)` rather than
/// private for that second caller; calling this twice for the same OAuth
/// failure is harmless, see that function's own doc.
pub(crate) async fn mark_needs_sign_in(vault: &Arc<Vault>, account_id: AccountId, why: &str) {
    let vault = vault.clone();
    let reason = why.to_string();
    let outcome = blocking(move || {
        let mut account = vault.account(account_id)?;
        account.status = AccountStatus::NeedsSignIn { reason };
        account.updated_at = Timestamp::now();
        Ok(vault.save_account(&account)?)
    })
    .await;
    if let Err(e) = outcome {
        tracing::warn!(%account_id, error = %e, "could not record that an account needs to sign in again");
    }
}

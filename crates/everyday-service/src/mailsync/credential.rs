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

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::{Account, AccountStatus, AuthMethod};
use everyday_mail::oauth::{OAuthClient, OAuthError};
use everyday_mail::session::Credential;
use jiff::Timestamp;

use crate::service::Service;

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
            let Some(refresh_token) = secret.refresh_token.clone() else {
                return Resolved::NeedsSignIn("this account has never signed in with OAuth".into());
            };
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
            let account_id = account.id;
            let refresh_token_for_call = refresh_token.clone();
            let result = svc
                .token_cache()
                .access_token(&account_id.to_string(), move || {
                    let vault = vault_for_rotation.clone();
                    let client = client.clone();
                    let refresh_token = refresh_token_for_call.clone();
                    async move {
                        let tokens = client.refresh(&refresh_token).await?;
                        // Persisted here, not by the caller, because this is
                        // the one place that knows whether the provider
                        // actually rotated it -- see `Tokens::refresh_token`'s
                        // own docs and `TokenCache`'s module doc on why the
                        // token cache itself never sees this value.
                        if tokens.refresh_token.is_some() {
                            let _ = persist_rotated_refresh_token(&vault, account_id, &tokens);
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

fn persist_rotated_refresh_token(
    vault: &Vault,
    account_id: everyday_core::id::AccountId,
    tokens: &everyday_mail::oauth::Tokens,
) -> everyday_core::Result<()> {
    let mut secret = vault.account_secret(account_id)?.unwrap_or_default();
    secret.refresh_token = tokens.refresh_token.clone();
    secret.access_token = Some((tokens.access_token.clone(), tokens.expires_at));
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

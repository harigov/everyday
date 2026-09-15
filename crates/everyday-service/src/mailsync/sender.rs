//! A lazily-connected, pooled SMTP sender for one account's sync task.
//!
//! `docs/plans/mail.md`'s phase 3 asks for exactly this: "Send needs an SMTP
//! transport: build it from the account's `smtp` endpoint and the same
//! credential resolution (XOAUTH2 or password), created lazily on first send
//! and kept pooled." [`everyday_mail::outbox::execute`] only ever calls
//! [`everyday_mail::outbox::Sender::send`] for
//! [`everyday_core::mail::OpKind::Send`], so an account whose outbox never
//! sends -- the common case; most drains are flag changes and archives --
//! never opens an SMTP connection at all. [`LazySmtpSender`] is exactly the
//! [`everyday_mail::outbox::Sender`] `crate::outbox::drain_outbox` is generic
//! over, held for the life of the account task that owns it, so the
//! connection inside it does not exist until the first call to
//! [`LazySmtpSender::send`] actually needs one -- and, once opened, is kept
//! and reused by every later one.
//!
//! # Why a fresh credential resolution on every connect, not just the first
//!
//! [`crate::mailsync::credential::resolve`] is cheap to call again: an OAuth
//! access token is cached by [`crate::token_cache::TokenCache`] and only
//! refreshed when it is close to expiring, so calling it here costs nothing
//! extra in the ordinary case and is exactly right in the case that matters
//! -- a connection opened after the access token this task authenticated
//! IMAP with an hour ago has since gone stale.
//!
//! # Why the cached transport is dropped on an authentication failure, and
//! # retried once for OAuth
//!
//! `lettre`'s own pool authenticates once per pooled connection, with
//! whatever credential the transport was built with -- it does not refresh
//! one mid-flight. An `AUTH` a live connection accepted an hour ago can
//! therefore start failing without anything in this module changing, the
//! moment the access token it was built with expires. [`LazySmtpSender::send`]
//! drops the cached transport the moment the server refuses authentication,
//! so a *later* send at least rebuilds one from a freshly resolved
//! credential instead of repeating a doomed `AUTH`.
//!
//! For OAuth accounts that is nearly always enough to fix on the spot,
//! rather than merely on the next call: [`crate::token_cache::TokenCache`]
//! caches an access token for its whole nominal lifetime, so the moment this
//! module actually discovers one has gone stale -- the server just said so
//! -- is also the first moment anything knew to distrust it. So `send`
//! forgets the cached token, resolves a fresh one (a real refresh, since the
//! cache is now empty for this account), rebuilds the transport, and retries
//! the same send exactly once before giving up. Only a *second* `AUTH`
//! failure, on a token that was just refreshed, means the grant itself is
//! bad -- a revoked or expired refresh token -- rather than an access token
//! that simply outlived its hour between IMAP's own refresh and this SMTP
//! connection getting around to needing one. A password account has no
//! token to refresh, so it is not retried: the same password would only
//! fail the same way again.

use std::future::Future;
use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::{Account, AuthMethod, EndpointSecurity};
use everyday_mail::compose::Built;
use everyday_mail::imap::Security;
use everyday_mail::outbox::Sender;
use everyday_mail::session::{Credential, MailError, Result};
use everyday_mail::smtp::{self, SendReceipt, SmtpTransport};
use tokio::sync::Mutex;

use crate::mailsync::credential::{self, Resolved};
use crate::service::Service;

/// One account's SMTP sender. See the module docs.
pub struct LazySmtpSender {
    account: Account,
    svc: Arc<Service>,
    vault: Arc<Vault>,
    cached: Mutex<Option<SmtpTransport>>,
}

impl LazySmtpSender {
    pub fn new(account: Account, svc: Arc<Service>, vault: Arc<Vault>) -> Self {
        Self { account, svc, vault, cached: Mutex::new(None) }
    }

    async fn credential(&self) -> Result<Credential> {
        match credential::resolve(&self.svc, &self.vault, &self.account).await {
            Resolved::Ready(c) => Ok(c),
            Resolved::NeedsSignIn(reason) => Err(MailError::Auth(reason)),
            Resolved::Transient(message) => Err(MailError::Network(message)),
        }
    }

    /// A fresh transport from a freshly resolved credential -- what both the
    /// first connection and the one retry [`Sender::send`] allows itself
    /// after a stale OAuth token both need.
    async fn connect(&self) -> Result<SmtpTransport> {
        let cred = self.credential().await?;
        let security = match self.account.smtp.security {
            EndpointSecurity::Tls => Security::Tls,
            EndpointSecurity::StartTls => Security::StartTls,
        };
        smtp::connect(&self.account.smtp.host, self.account.smtp.port, security, &cred).await
    }
}

#[allow(async_fn_in_trait)]
impl Sender for LazySmtpSender {
    async fn send(&self, built: &Built) -> Result<SendReceipt> {
        let is_oauth = matches!(self.account.auth, AuthMethod::OAuth { .. });
        let account_key = self.account.id.to_string();
        send_with_retry(
            &self.cached,
            is_oauth,
            || self.connect(),
            || async { self.svc.token_cache().forget(&account_key).await },
            built,
        )
        .await
    }
}

/// [`LazySmtpSender::send`]'s whole retry algorithm, generic over how a
/// transport is built (`connect`) and how a stale cached token is forgotten
/// (`forget`) -- see the module docs for what it does and why. Pulled out
/// of the trait `impl` so a test can drive the exact algorithm against
/// fakes for both, with no socket and no real OAuth endpoint anywhere in
/// reach; [`LazySmtpSender::send`] is the one production caller, wiring
/// `connect` to [`LazySmtpSender::connect`] and `forget` to
/// [`crate::token_cache::TokenCache::forget`].
async fn send_with_retry<T, Conn, ConnFut, Forget, ForgetFut>(
    cached: &Mutex<Option<T>>,
    is_oauth: bool,
    mut connect: Conn,
    mut forget: Forget,
    built: &Built,
) -> Result<SendReceipt>
where
    T: Sender,
    Conn: FnMut() -> ConnFut,
    ConnFut: Future<Output = Result<T>>,
    Forget: FnMut() -> ForgetFut,
    ForgetFut: Future<Output = ()>,
{
    let mut guard = cached.lock().await;
    if guard.is_none() {
        *guard = Some(connect().await?);
    }
    // The branch above just filled it, and nothing else here clears it
    // while this guard is held.
    let transport = guard.as_ref().expect("just connected");
    match transport.send(built).await {
        Err(MailError::Auth(reason)) => {
            // The cached connection's credential no longer works -- drop it
            // so a rebuilt transport is never reused past this point. See
            // the module docs.
            *guard = None;
            if !is_oauth {
                return Err(MailError::Auth(reason));
            }
            forget().await;
            let transport = connect().await?;
            let retry = transport.send(built).await;
            if retry.is_ok() {
                *guard = Some(transport);
            }
            retry
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_mail::smtp::SendReceipt;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn built() -> Built {
        Built {
            raw: b"From: a@example.com\r\n\r\nhi\r\n".to_vec(),
            message_id: "msg-1@example.com".into(),
            envelope_from: "a@example.com".into(),
            envelope_to: vec!["b@example.com".into()],
        }
    }

    /// A transport that fails every `send` with [`MailError::Auth`] until
    /// `succeeds_from_call` (1-indexed, across every transport this test's
    /// `connect` hands out, not just this one) -- standing in for the
    /// stale-then-fresh pooled connection [`send_with_retry`] exists for.
    struct FakeTransport {
        send_calls: Arc<AtomicU32>,
        accepted: Arc<AtomicU32>,
        succeeds_from_call: u32,
    }

    #[allow(async_fn_in_trait)]
    impl Sender for FakeTransport {
        async fn send(&self, _built: &Built) -> Result<SendReceipt> {
            let call = self.send_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call < self.succeeds_from_call {
                return Err(MailError::Auth("token expired".into()));
            }
            self.accepted.fetch_add(1, Ordering::SeqCst);
            Ok(SendReceipt {
                accepted: vec!["b@example.com".into()],
                server_response: "250 Ok".into(),
            })
        }
    }

    #[tokio::test]
    async fn an_oauth_account_retries_once_on_auth_and_sends_exactly_once() {
        let cached: Mutex<Option<FakeTransport>> = Mutex::new(None);
        let send_calls = Arc::new(AtomicU32::new(0));
        let accepted = Arc::new(AtomicU32::new(0));
        let connects = Arc::new(AtomicU32::new(0));
        let forgets = Arc::new(AtomicU32::new(0));

        let connect = || {
            connects.fetch_add(1, Ordering::SeqCst);
            let send_calls = send_calls.clone();
            let accepted = accepted.clone();
            async move {
                Ok::<_, MailError>(FakeTransport {
                    send_calls,
                    accepted,
                    // The first transport's send fails with Auth; the one
                    // built after `forget` succeeds -- exactly the pooled
                    // connection holding an expired token this module
                    // exists to recover from.
                    succeeds_from_call: 2,
                })
            }
        };
        let forget = || {
            forgets.fetch_add(1, Ordering::SeqCst);
            async {}
        };

        let receipt = send_with_retry(&cached, true, connect, forget, &built()).await.unwrap();
        assert_eq!(receipt.server_response, "250 Ok");
        assert_eq!(send_calls.load(Ordering::SeqCst), 2, "one failed attempt, one retry");
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            1,
            "the message must reach the server exactly once"
        );
        assert_eq!(connects.load(Ordering::SeqCst), 2, "the stale transport, then a rebuilt one");
        assert_eq!(
            forgets.load(Ordering::SeqCst),
            1,
            "the stale token must be forgotten before the retry"
        );
    }

    #[tokio::test]
    async fn a_password_account_is_not_retried() {
        let cached: Mutex<Option<FakeTransport>> = Mutex::new(None);
        let send_calls = Arc::new(AtomicU32::new(0));
        let accepted = Arc::new(AtomicU32::new(0));
        let forgets = Arc::new(AtomicU32::new(0));

        let connect = {
            let send_calls = send_calls.clone();
            let accepted = accepted.clone();
            move || {
                let send_calls = send_calls.clone();
                let accepted = accepted.clone();
                async move {
                    Ok::<_, MailError>(FakeTransport {
                        send_calls,
                        accepted,
                        succeeds_from_call: 2,
                    })
                }
            }
        };
        let forget = || {
            forgets.fetch_add(1, Ordering::SeqCst);
            async {}
        };

        let err = send_with_retry(&cached, false, connect, forget, &built()).await.unwrap_err();
        assert!(matches!(err, MailError::Auth(_)));
        assert_eq!(send_calls.load(Ordering::SeqCst), 1, "no retry without OAuth to refresh");
        assert_eq!(forgets.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_second_auth_failure_after_the_retry_is_surfaced() {
        let cached: Mutex<Option<FakeTransport>> = Mutex::new(None);
        let send_calls = Arc::new(AtomicU32::new(0));
        let accepted = Arc::new(AtomicU32::new(0));

        let connect = {
            let send_calls = send_calls.clone();
            let accepted = accepted.clone();
            move || {
                let send_calls = send_calls.clone();
                let accepted = accepted.clone();
                // Never succeeds: the freshly refreshed token is bad too --
                // a genuinely revoked grant, not mere staleness.
                async move {
                    Ok::<_, MailError>(FakeTransport {
                        send_calls,
                        accepted,
                        succeeds_from_call: u32::MAX,
                    })
                }
            }
        };
        let forget = || async {};

        let err = send_with_retry(&cached, true, connect, forget, &built()).await.unwrap_err();
        assert!(matches!(err, MailError::Auth(_)));
        assert_eq!(send_calls.load(Ordering::SeqCst), 2, "exactly one retry, not a loop");
        assert_eq!(accepted.load(Ordering::SeqCst), 0);
    }
}

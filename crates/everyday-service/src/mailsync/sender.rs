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
//! # Why the cached transport is dropped on an authentication failure
//!
//! `lettre`'s own pool authenticates once per pooled connection, with
//! whatever credential the transport was built with -- it does not refresh
//! one mid-flight. An `AUTH` a live connection accepted an hour ago can
//! therefore start failing without anything in this module changing, the
//! moment the access token it was built with expires. Rather than surface
//! that same failure for ever, [`LazySmtpSender::send`] drops the cached
//! transport the moment the server refuses authentication, so the *next*
//! send rebuilds one from a freshly resolved credential instead of repeating
//! a doomed `AUTH`.

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::{Account, EndpointSecurity};
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
}

#[allow(async_fn_in_trait)]
impl Sender for LazySmtpSender {
    async fn send(&self, built: &Built) -> Result<SendReceipt> {
        let mut guard = self.cached.lock().await;
        if guard.is_none() {
            let cred = self.credential().await?;
            let security = match self.account.smtp.security {
                EndpointSecurity::Tls => Security::Tls,
                EndpointSecurity::StartTls => Security::StartTls,
            };
            let transport =
                smtp::connect(&self.account.smtp.host, self.account.smtp.port, security, &cred)
                    .await?;
            *guard = Some(transport);
        }
        // The branch above just filled it, and nothing else here clears it
        // while this guard is held.
        let transport = guard.as_ref().expect("just connected");
        match transport.send(built).await {
            Err(MailError::Auth(reason)) => {
                // The cached connection's credential no longer works -- drop
                // it so the next send resolves a fresh one. See the module
                // docs.
                *guard = None;
                Err(MailError::Auth(reason))
            }
            other => other,
        }
    }
}

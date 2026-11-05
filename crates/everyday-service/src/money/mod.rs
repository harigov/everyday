//! Money's moving parts: talking to Plaid, the sign-in that links a bank,
//! the refresher that keeps linked accounts current, and reading a bank's
//! export file. See `docs/plans/money.md`.
//!
//! ```text
//!   plaid      the API: fixed hosts, no redirects, credentials only in
//!              request bodies, every answer size-capped; Plaid's sign
//!              convention turned round here and nowhere else
//!   link       Hosted Link: a sign-in page Plaid serves, opened in the
//!              person's own browser, polled until it finishes -- so no
//!              Plaid script ever runs in this application's webview
//!   refresh    one link's sync, and the supervised loop that gives every
//!              link its turn
//!   import     OFX/QFX and CSV, pure: text in, `ImportedTxn`s out
//! ```
//!
//! # What may leave the vault, and to where
//!
//! The Plaid credentials and a link's access token go to Plaid's own API
//! host for the environment they belong to and nowhere else: the host is a
//! constant ([`everyday_core::money::PlaidEnvironment::base_url`]), not a
//! setting, so nothing can point them at a server of somebody else's
//! choosing. Nothing in this module logs a request body, and every error
//! that reaches a person or the vault is Plaid's own error message, never a
//! request.

pub mod import;
pub mod link;
pub mod plaid;
pub mod refresh;

use crate::service::Service;
use everyday_core::Vault;
use everyday_core::id::BankLinkId;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The supervisor key of the one refresher task.
pub const TASK_KEY: &str = "money";

/// What one link's sync did -- what `sync_bank_links` answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub link_id: BankLinkId,
    pub added: u32,
    pub updated: u32,
    pub removed: u32,
    /// The bank wants the person to sign in again.
    pub needs_login: bool,
    /// Why it failed, in words for a person, nothing secret. `None` on
    /// success.
    pub error: Option<String>,
}

impl SyncReport {
    pub fn failed(link_id: BankLinkId, error: impl Into<String>) -> Self {
        Self {
            link_id,
            added: 0,
            updated: 0,
            removed: 0,
            needs_login: false,
            error: Some(error.into()),
        }
    }

    /// Did this sync change anything a list would show?
    pub fn changed(&self) -> bool {
        self.added + self.updated + self.removed > 0
    }
}

/// This session's Money state: Hosted Link sessions in flight, one lock per
/// link so two syncs of one link never race their cursors, and which links'
/// failures have already been said out loud. All of it goes when the vault
/// locks -- a link session holds a Plaid link token, which is a bearer
/// credential for the few minutes it lives.
#[derive(Default)]
pub struct MoneyRuntime {
    pub(crate) sessions: Mutex<HashMap<String, link::Session>>,
    sync_turns: Mutex<HashMap<BankLinkId, Arc<tokio::sync::Mutex<()>>>>,
    reported: Mutex<HashSet<BankLinkId>>,
}

impl MoneyRuntime {
    /// Held while `id` syncs, by the refresher and by "Sync now" alike.
    pub(crate) fn sync_turn(&self, id: BankLinkId) -> Arc<tokio::sync::Mutex<()>> {
        self.sync_turns.lock().unwrap_or_else(|p| p.into_inner()).entry(id).or_default().clone()
    }

    /// Note that `id` failed. True the first time, so an outage is said once.
    pub(crate) fn failed(&self, id: BankLinkId) -> bool {
        self.reported.lock().unwrap_or_else(|p| p.into_inner()).insert(id)
    }

    /// Note that `id` synced, so its next outage is news again.
    pub(crate) fn recovered(&self, id: BankLinkId) {
        self.reported.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
    }

    /// Forget everything: the vault locked or closed.
    pub(crate) fn on_lock(&self) {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner()).clear();
        // The sync turns are kept: they hold no secret, and a "Sync now"
        // still answering when the vault locked holds one. Dropped here, a
        // quick unlock would hand the refresher a fresh lock for the same
        // link and let the two race its cursor.
        self.reported.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

/// Write the starter categories if the vault has none, then start the
/// refresher if it is not already running. Idempotent -- see
/// [`crate::supervisor::Supervisor::ensure`] -- so it is called from every
/// unlock and costs nothing when the task is already there.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if !vault.supports_money() {
        return;
    }
    if let Err(e) = vault.ensure_money_defaults() {
        tracing::warn!("money: could not write the starter categories: {e}");
    }
    // Looks the vault up each time it starts rather than keeping one, for
    // the reason `news::ensure_task` gives.
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        Box::pin(async move {
            match svc.get() {
                Some(vault) if vault.supports_money() => refresh::run(svc, vault, stop).await,
                _ => Ok(crate::supervisor::Outcome::Done),
            }
        })
    });
}

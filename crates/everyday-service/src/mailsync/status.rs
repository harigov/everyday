//! What `sync_status` reads: each account's current phase, progress and
//! last error.
//!
//! Kept in memory, not in the vault -- this is exactly as disposable as the
//! [`crate::supervisor::Supervisor`]'s own [`TaskState`](crate::supervisor::TaskState)
//! it sits beside, and for the same reason: it is a fact about *this
//! session's* attempt, not a vault record anything else reads. A restart
//! sees every account as [`Phase::Idle`] until its task's first attempt
//! reports in, which is the honest answer -- the previous session's numbers
//! describe a sync that is no longer running.

use everyday_core::id::AccountId;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// Which part of the sync an account's task is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Registered, but its task has not reported progress yet -- starting
    /// up, or between attempts under the supervisor's backoff.
    Idle,
    Connecting,
    /// Discovering mailboxes and syncing headers -- the first of the three
    /// first-sync passes, and the one that makes a mailbox list usable.
    Headers,
    /// Fetching and indexing bodies -- the second pass.
    Bodies,
    /// Extracting attachments -- the third pass.
    Attachments,
    /// Holding `IDLE` and polling the rest of the account's mailboxes.
    Idling,
}

/// One account's progress, as `sync_status` reports it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub account_id: AccountId,
    pub phase: Phase,
    /// How many units of the current phase are done, and how many there are
    /// in total -- messages headers fetched, bodies indexed, and so on.
    /// `total` is `0` when it is not yet known (before the UID list for a
    /// mailbox has been read), which a caller reads as "in progress,
    /// indeterminate" rather than "nothing to do".
    pub done: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl Progress {
    fn idle(account_id: AccountId) -> Self {
        Self { account_id, phase: Phase::Idle, done: 0, total: 0, last_error: None }
    }
}

/// Every account task's progress, shared between the tasks that write it and
/// the `sync_status` command that reads it -- and the "nudge" channel
/// `sync_account` uses to wake a task that is sitting in `IDLE` or its poll
/// sleep, so "force an immediate pass" does not mean "wait up to
/// `POLL_INTERVAL`".
#[derive(Clone, Default)]
pub struct StatusRegistry(Arc<Shared>);

#[derive(Default)]
struct Shared {
    progress: Mutex<HashMap<AccountId, Progress>>,
    nudges: Mutex<HashMap<AccountId, watch::Sender<()>>>,
}

impl StatusRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The receiver a task's steady-state loop selects on -- created the
    /// first time an account is asked for, so `sync_account` calling
    /// [`StatusRegistry::nudge`] before the task has ever subscribed is not
    /// a missed wake-up, only one with nothing listening yet.
    pub fn subscribe(&self, account_id: AccountId) -> watch::Receiver<()> {
        self.0
            .nudges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(account_id)
            .or_insert_with(|| watch::channel(()).0)
            .subscribe()
    }

    /// Wake `account_id`'s task early. A no-op if nothing has subscribed --
    /// the task is not running, and starting it (via `sync_account`'s own
    /// call to `ensure_account_task`) already gives it an immediate pass
    /// with nothing to nudge.
    pub fn nudge(&self, account_id: AccountId) {
        if let Some(tx) = self.0.nudges.lock().unwrap_or_else(|e| e.into_inner()).get(&account_id) {
            let _ = tx.send(());
        }
    }

    /// Set an account's phase and progress within it, clearing any earlier
    /// error -- a phase only advances once whatever blocked the last one has
    /// stopped blocking it.
    pub fn set_phase(&self, account_id: AccountId, phase: Phase, done: u64, total: u64) {
        let mut map = self.0.progress.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(account_id).or_insert_with(|| Progress::idle(account_id));
        entry.phase = phase;
        entry.done = done;
        entry.total = total;
        entry.last_error = None;
    }

    /// Record an error without changing the phase -- a network hiccup mid-pass
    /// is still in that pass when the supervisor retries it.
    pub fn set_error(&self, account_id: AccountId, message: String) {
        let mut map = self.0.progress.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(account_id).or_insert_with(|| Progress::idle(account_id));
        entry.last_error = Some(message);
    }

    /// Move an account back to [`Phase::Idle`] -- called when its task stops,
    /// so a locked vault's `sync_status` does not go on claiming a sync that
    /// is not running.
    pub fn set_idle(&self, account_id: AccountId) {
        let mut map = self.0.progress.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(account_id, Progress::idle(account_id));
    }

    /// Forget an account entirely -- called when it is deleted.
    pub fn forget(&self, account_id: AccountId) {
        self.0.progress.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id);
        self.0.nudges.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id);
    }

    /// Every account this session has ever reported on.
    pub fn all(&self) -> Vec<Progress> {
        self.0.progress.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }
}

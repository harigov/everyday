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
use jiff::Timestamp;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
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
    /// Discovering mailboxes and syncing headers -- the first of the two
    /// first-sync passes, and the one that makes a mailbox list usable.
    Headers,
    /// Fetching and indexing bodies -- the second pass. There is no third
    /// phase for attachments: `crate::mailsync::passes`'s own module docs
    /// explain why extracting them is folded into this same pass rather
    /// than a separate walk over the mailbox, so nothing ever reports a
    /// phase beyond this one.
    Bodies,
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
    /// When this session last finished a whole pass over the account --
    /// every mailbox's headers and bodies, through to `mark_ok`. What lets
    /// the interface say "checked a minute ago" without reading the account
    /// record, whose own `last_synced_at` says the same thing durably but
    /// only reaches a window that reloads the account list. `None` until
    /// the first pass of this session lands, on the same "a previous
    /// session's numbers describe a sync that is no longer running"
    /// reasoning as the rest of this module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<Timestamp>,
}

impl Progress {
    fn idle(account_id: AccountId) -> Self {
        Self {
            account_id,
            phase: Phase::Idle,
            done: 0,
            total: 0,
            last_error: None,
            last_synced_at: None,
        }
    }
}

/// Every account task's progress, shared between the tasks that write it and
/// the `sync_status` command that reads it -- and the "nudge" channel
/// `sync_account` (and `save_account`, for a changed interval) uses to wake
/// a task that is sitting in `IDLE` or its poll sleep, so "force an
/// immediate pass" does not mean "wait out the account's sync interval" --
/// and the "something changed" flag a pass leaves for its own task to
/// announce once it is over.
#[derive(Clone, Default)]
pub struct StatusRegistry(Arc<Shared>);

#[derive(Default)]
struct Shared {
    progress: Mutex<HashMap<AccountId, Progress>>,
    nudges: Mutex<HashMap<AccountId, watch::Sender<()>>>,
    /// Accounts whose sync has written something a thread list would show
    /// since the account task last asked -- see [`StatusRegistry::mark_changed`].
    changed: Mutex<HashSet<AccountId>>,
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
    /// is not running. Everything else about the entry is reset with it
    /// except `last_synced_at`: the pass that finished still finished, and
    /// "last checked at ten past" stays true of a task that has since
    /// stopped.
    pub fn set_idle(&self, account_id: AccountId) {
        let mut map = self.0.progress.lock().unwrap_or_else(|e| e.into_inner());
        let last_synced_at = map.get(&account_id).and_then(|p| p.last_synced_at);
        let mut idle = Progress::idle(account_id);
        idle.last_synced_at = last_synced_at;
        map.insert(account_id, idle);
    }

    /// Record that a whole pass over `account_id` finished at `at` -- see
    /// [`Progress::last_synced_at`]. Leaves the phase and any error alone:
    /// the account task sets those itself, a line either side of this.
    pub fn set_synced(&self, account_id: AccountId, at: Timestamp) {
        let mut map = self.0.progress.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(account_id).or_insert_with(|| Progress::idle(account_id));
        entry.last_synced_at = Some(at);
    }

    /// Note that a sync pass over `account_id` wrote something a thread list
    /// draws -- a new message, a flag another client changed, a message
    /// gone -- so the account task, once the pass is over, knows to tell an
    /// open window. Called from inside the pass (once per mailbox that
    /// changed, so possibly several times) and read once at its end by
    /// [`StatusRegistry::take_changed`]; a flag rather than a counter
    /// because the window only ever needs to hear "reload", once.
    pub fn mark_changed(&self, account_id: AccountId) {
        self.0.changed.lock().unwrap_or_else(|e| e.into_inner()).insert(account_id);
    }

    /// Whether [`StatusRegistry::mark_changed`] was called for `account_id`
    /// since the last time this was asked, clearing it as it answers -- so
    /// a pass that changed nothing announces nothing, and one that changed
    /// forty threads announces once.
    pub fn take_changed(&self, account_id: AccountId) -> bool {
        self.0.changed.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id)
    }

    /// Forget an account entirely -- called when it is deleted.
    pub fn forget(&self, account_id: AccountId) {
        self.0.progress.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id);
        self.0.nudges.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id);
        self.0.changed.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id);
    }

    /// Every account this session has ever reported on.
    pub fn all(&self) -> Vec<Progress> {
        self.0.progress.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress_of(registry: &StatusRegistry, account_id: AccountId) -> Progress {
        registry.all().into_iter().find(|p| p.account_id == account_id).expect("reported on")
    }

    #[test]
    fn a_change_is_taken_once_and_only_for_its_own_account() {
        let registry = StatusRegistry::new();
        let (a, b) = (AccountId::new(), AccountId::new());
        assert!(!registry.take_changed(a), "nothing has changed yet");

        registry.mark_changed(a);
        registry.mark_changed(a);
        assert!(!registry.take_changed(b), "another account's change is not this one's");
        assert!(registry.take_changed(a), "marked twice, taken once");
        assert!(!registry.take_changed(a), "taking clears it");
    }

    #[test]
    fn forgetting_an_account_drops_a_change_nobody_took() {
        let registry = StatusRegistry::new();
        let a = AccountId::new();
        registry.mark_changed(a);
        registry.forget(a);
        assert!(!registry.take_changed(a));
    }

    #[test]
    fn the_last_sync_survives_a_new_phase_and_the_task_stopping() {
        let registry = StatusRegistry::new();
        let a = AccountId::new();
        let at = Timestamp::from_second(1_790_000_000).unwrap();

        registry.set_phase(a, Phase::Headers, 1, 10);
        registry.set_synced(a, at);
        registry.set_phase(a, Phase::Idling, 0, 0);
        assert_eq!(progress_of(&registry, a).last_synced_at, Some(at));

        registry.set_error(a, "the network went away".into());
        registry.set_idle(a);
        let idle = progress_of(&registry, a);
        assert_eq!(idle.phase, Phase::Idle);
        assert_eq!(idle.last_error, None, "idle resets everything else");
        assert_eq!(idle.last_synced_at, Some(at), "but a pass that finished still finished");
    }

    #[test]
    fn the_last_sync_is_on_the_wire_only_once_there_is_one() {
        let registry = StatusRegistry::new();
        let a = AccountId::new();
        registry.set_phase(a, Phase::Connecting, 0, 0);
        let wire = serde_json::to_value(progress_of(&registry, a)).unwrap();
        assert!(wire.get("lastSyncedAt").is_none(), "{wire}");

        registry.set_synced(a, Timestamp::from_second(1_790_000_000).unwrap());
        let wire = serde_json::to_value(progress_of(&registry, a)).unwrap();
        assert!(wire["lastSyncedAt"].is_string(), "{wire}");
    }
}

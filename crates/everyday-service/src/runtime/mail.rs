//! Mail's session state -- open for exactly as long as the vault it
//! belongs to is unlocked, plus the bookkeeping around it that lives for
//! the whole process regardless. See [`Service::open_mail`](crate::service::Service::open_mail)/
//! [`Service::close_mail`](crate::service::Service::close_mail) for the two
//! moments the state itself opens and drops, and [`MailRuntime::on_lock`]
//! for the extra clearing only a full vault close does -- `locked()` (a
//! lock screen; the same vault is still the one to reopen) does not call
//! it, and that gap is deliberate. See `tests/runtime_lifecycle.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use everyday_core::id::{AccountId, DraftId, ThreadId};
use everyday_core::mail::rate_limit::RateLimitRefusal;
use everyday_core::mail::{RateLimitState, TokenBucket};
use jiff::{SignedDuration, Timestamp};

use crate::mailsync::wiring::MailState;

pub(crate) struct MailRuntime {
    /// Mail's pack store and search index, and each account's sync
    /// progress -- open and populated for exactly as long as the vault they
    /// belong to is unlocked. See `mailsync::wiring` for how it is opened
    /// (with a key derived from the vault's own, never reused) and
    /// [`MailRuntime::set_state`] for the door `mailsync::wiring` reaches it
    /// through.
    state: RwLock<Option<MailState>>,
    /// One [`tokio::sync::Notify`] per account, woken by
    /// [`crate::service::Service::notify_outbox`] whenever a write enqueues
    /// an `Op` -- what lets an account's sync task drain the outbox the
    /// moment something is due rather than waiting for its next `IDLE` wake
    /// or timer tick. Get-or-create through [`MailRuntime::outbox_notify`],
    /// so whichever of a write command or the sync task asks first creates
    /// the handle the other one shares.
    notify: Mutex<HashMap<AccountId, Arc<tokio::sync::Notify>>>,
    /// When each draft last enqueued an `AppendDraft` op, for
    /// [`MailRuntime::draft_append_due`]'s thirty-second debounce. Session
    /// state, not a vault fact: a draft typed into for a minute autosaves
    /// locally on every keystroke, and this is what stops each of those
    /// saves from also appending to the server's Drafts folder.
    draft_debounce: Mutex<HashMap<DraftId, Timestamp>>,
    /// Per-caller state for [`MailRuntime::check_rate_limit`], keyed by
    /// `Origin::Assistant`'s conversation or `Origin::Mcp`'s client name.
    /// `Origin::Person` and `Origin::Routine` never appear here --
    /// [`Origin::is_rate_limited`](everyday_core::mail::Origin::is_rate_limited)
    /// says so, and [`MailRuntime::check_rate_limit`] returns before ever
    /// touching this map for either.
    rate_limits: Mutex<HashMap<String, RateLimitState>>,
    /// Paces `everyday_service::mailai`'s background model-assisted
    /// categorisation pass: at most a handful of threads sent to the quick
    /// model per rolling minute, across every account, per the plan's own
    /// words ("at most N threads per minute"). Session state, on the same
    /// terms every other mail limiter here is -- a restart simply starts a
    /// fresh minute's budget. Deliberately *not* cleared by
    /// [`MailRuntime::on_lock`] -- see that method's own doc.
    categorize_budget: Mutex<TokenBucket>,
    /// As [`MailRuntime::categorize_budget`], for the auto-draft background
    /// pass.
    autodraft_budget: Mutex<TokenBucket>,
    /// `summarize_thread`'s cache: a thread's summary, keyed by how many
    /// messages it had when it was written. A thread that has grown since
    /// -- a new message landed -- misses the cache and is summarised again;
    /// one that has not is answered instantly. In memory, not the vault:
    /// losing it on a restart costs one re-summarise, never data nothing
    /// else remembers, the same trade `notify` and the rest of this session
    /// state already make.
    summary_cache: Mutex<HashMap<ThreadId, (u32, String)>>,
    /// Where [`everyday_service::mailai::categorize_tick`]'s next pass over
    /// an account's `Other` threads should start -- see the field this
    /// replaces, `Service::mail_categorize_cursor`, for the full account of
    /// why this has to advance every tick and wrap on an empty page.
    categorize_cursor: Mutex<HashMap<AccountId, String>>,
    /// As [`MailRuntime::categorize_cursor`], for the auto-draft pass over
    /// `Important` threads.
    autodraft_cursor: Mutex<HashMap<AccountId, String>>,
}

impl MailRuntime {
    /// Re-seed both token buckets to `now`.
    ///
    /// For [`Service::set_clock`](crate::service::Service::set_clock) only.
    /// A bucket remembers when it last refilled, and
    /// [`TokenBucket::refill`] deliberately refuses to refill from a moment
    /// earlier than that one -- so a service handed a clock set in the past
    /// would spend its initial capacity and then hand out nothing for the
    /// rest of the run, silently. Re-seeding is what makes a fake clock
    /// behave like a freshly-built service rather than a stuck one.
    pub(crate) fn reseed_budgets(&self, now: Timestamp) {
        self.categorize_budget.lock().unwrap().reseed(now);
        self.autodraft_budget.lock().unwrap().reseed(now);
    }

    pub(crate) fn new(
        now: Timestamp,
        categorize_per_minute: u32,
        autodraft_per_minute: u32,
    ) -> Self {
        Self {
            state: RwLock::new(None),
            notify: Mutex::new(HashMap::new()),
            draft_debounce: Mutex::new(HashMap::new()),
            rate_limits: Mutex::new(HashMap::new()),
            categorize_budget: Mutex::new(TokenBucket::new(
                categorize_per_minute,
                f64::from(categorize_per_minute) / 60.0,
                now,
            )),
            autodraft_budget: Mutex::new(TokenBucket::new(
                autodraft_per_minute,
                f64::from(autodraft_per_minute) / 60.0,
                now,
            )),
            summary_cache: Mutex::new(HashMap::new()),
            categorize_cursor: Mutex::new(HashMap::new()),
            autodraft_cursor: Mutex::new(HashMap::new()),
        }
    }

    // ---- the open state itself --------------------------------------

    pub(crate) fn packs(&self) -> Option<Arc<dyn everyday_core::packstore::PackStore>> {
        self.state.read().unwrap().as_ref().map(|m| m.packs.clone())
    }

    pub(crate) fn index(&self) -> Option<Arc<dyn everyday_core::MailSearch>> {
        self.state.read().unwrap().as_ref().map(|m| m.index.clone())
    }

    pub(crate) fn statuses(&self) -> Option<crate::mailsync::status::StatusRegistry> {
        self.state.read().unwrap().as_ref().map(|m| m.statuses.clone())
    }

    pub(crate) fn unread_cache(&self) -> Option<Arc<crate::mailsync::unread_cache::UnreadCache>> {
        self.state.read().unwrap().as_ref().map(|m| m.unread_cache.clone())
    }

    pub(crate) fn contacts(&self) -> Option<Arc<crate::mailsync::contacts::ContactIndex>> {
        self.state.read().unwrap().as_ref().map(|m| m.contacts.clone())
    }

    /// Replace what [`MailRuntime::packs`], [`MailRuntime::index`] and
    /// [`MailRuntime::statuses`] answer -- `mailsync::wiring`'s own door
    /// into this state.
    pub(crate) fn set_state(&self, state: Option<MailState>) {
        *self.state.write().unwrap() = state;
    }

    // ---- the outbox ----------------------------------------------------

    pub(crate) fn outbox_notify(&self, account: AccountId) -> Arc<tokio::sync::Notify> {
        self.notify
            .lock()
            .unwrap()
            .entry(account)
            .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
            .clone()
    }

    /// Should this draft append to the server's Drafts folder right now?
    /// See the field this replaces, `Service::mail_draft_debounce`, for the
    /// coalescing this exists for.
    pub(crate) fn draft_append_due(&self, draft: DraftId, now: Timestamp) -> bool {
        const DEBOUNCE: SignedDuration = SignedDuration::from_secs(30);
        let mut last = self.draft_debounce.lock().unwrap();
        let due = match last.get(&draft) {
            Some(&previous) => now.duration_since(previous) >= DEBOUNCE,
            None => true,
        };
        if due {
            last.insert(draft, now);
        }
        due
    }

    /// The one gate every mail-op enqueue passes through -- see the method
    /// this replaces, `Service::check_mail_rate_limit`, for the full
    /// account of why `Origin::Person` and `Origin::Routine` never reach
    /// this map.
    pub(crate) fn check_rate_limit(
        &self,
        key: String,
        turn: &str,
        now: Timestamp,
    ) -> Result<(), RateLimitRefusal> {
        /// How many mail ops one model turn may enqueue before it is
        /// refused -- generous enough for "archive these dozen newsletters"
        /// in one go, tight enough that a runaway loop cannot spend a whole
        /// minute's budget in a single turn.
        const PER_TURN: u32 = 20;
        /// How many mail ops one caller may enqueue per rolling minute.
        const PER_MINUTE: u32 = 60;

        let mut limits = self.rate_limits.lock().unwrap();
        let state =
            limits.entry(key).or_insert_with(|| RateLimitState::new(PER_TURN, PER_MINUTE, now));
        state.check(turn, now)
    }

    // ---- the categorisation and auto-draft budgets ----------------------

    pub(crate) fn categorize_take(&self, now: Timestamp, want: u32) -> u32 {
        take_tokens(&self.categorize_budget, now, want)
    }

    pub(crate) fn autodraft_take(&self, now: Timestamp, want: u32) -> u32 {
        take_tokens(&self.autodraft_budget, now, want)
    }

    pub(crate) fn categorize_cursor(&self, account: AccountId) -> Option<String> {
        self.categorize_cursor.lock().unwrap().get(&account).cloned()
    }

    pub(crate) fn set_categorize_cursor(&self, account: AccountId, cursor: Option<String>) {
        set_cursor(&self.categorize_cursor, account, cursor);
    }

    pub(crate) fn autodraft_cursor(&self, account: AccountId) -> Option<String> {
        self.autodraft_cursor.lock().unwrap().get(&account).cloned()
    }

    pub(crate) fn set_autodraft_cursor(&self, account: AccountId, cursor: Option<String>) {
        set_cursor(&self.autodraft_cursor, account, cursor);
    }

    pub(crate) fn summary_cached(&self, thread: ThreadId, message_count: u32) -> Option<String> {
        let cache = self.summary_cache.lock().unwrap();
        cache.get(&thread).filter(|(n, _)| *n == message_count).map(|(_, s)| s.clone())
    }

    pub(crate) fn summary_cache_put(&self, thread: ThreadId, message_count: u32, summary: String) {
        self.summary_cache.lock().unwrap().insert(thread, (message_count, summary));
    }

    // ---- the extra clearing a full vault close does ---------------------
    //
    // `on_lock` bundles these six, called in this order, right after
    // `Service::close` closes mail's pack store and index -- unlike
    // `locked()` (a lock screen; the same vault is still the one to
    // reopen), which calls `on_lock` at all. Deliberately not
    // `categorize_budget`/`autodraft_budget`: a spent budget staying spent
    // across a close is what "a restart starts a fresh minute's budget"
    // (their own field docs) is contrasted against -- a *process* restart,
    // not this.

    fn forget_notify(&self) {
        self.notify.lock().unwrap().clear();
    }

    fn forget_draft_debounce(&self) {
        self.draft_debounce.lock().unwrap().clear();
    }

    fn forget_rate_limits(&self) {
        self.rate_limits.lock().unwrap().clear();
    }

    fn forget_summary_cache(&self) {
        self.summary_cache.lock().unwrap().clear();
    }

    fn forget_categorize_cursor(&self) {
        self.categorize_cursor.lock().unwrap().clear();
    }

    fn forget_autodraft_cursor(&self) {
        self.autodraft_cursor.lock().unwrap().clear();
    }

    /// Called once, by [`Service::close`](crate::service::Service::close),
    /// in place of the six `forget_*` calls above written out by hand.
    /// **Not** called by `Service::locked` -- see this module's own doc for
    /// why that gap is deliberate, and `tests/runtime_lifecycle.rs` for the
    /// test that pins it.
    pub(crate) fn on_lock(&self) {
        self.forget_notify();
        self.forget_draft_debounce();
        self.forget_rate_limits();
        self.forget_summary_cache();
        self.forget_categorize_cursor();
        self.forget_autodraft_cursor();
    }
}

fn set_cursor(
    cursors: &Mutex<HashMap<AccountId, String>>,
    account: AccountId,
    cursor: Option<String>,
) {
    let mut cursors = cursors.lock().unwrap();
    match cursor {
        Some(c) => {
            cursors.insert(account, c);
        }
        None => {
            cursors.remove(&account);
        }
    }
}

/// Take up to `want` tokens from `bucket`, one at a time, and say how many
/// were actually there -- what [`MailRuntime::categorize_take`] and
/// [`MailRuntime::autodraft_take`] both are.
fn take_tokens(bucket: &Mutex<TokenBucket>, now: Timestamp, want: u32) -> u32 {
    let mut bucket = bucket.lock().unwrap();
    let mut taken = 0u32;
    while taken < want && bucket.try_take(now) {
        taken += 1;
    }
    taken
}

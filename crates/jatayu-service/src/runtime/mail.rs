//! Mail's session state -- open for exactly as long as the vault it
//! belongs to is unlocked, plus the bookkeeping around it that lives for
//! the whole process regardless. See [`Service::open_mail`](crate::service::Service::open_mail)/
//! [`Service::close_mail`](crate::service::Service::close_mail) for the two
//! moments the state itself opens and drops, and [`MailRuntime::on_lock`]
//! for the extra clearing only a full vault close does -- `locked()` (a
//! lock screen; the same vault is still the one to reopen) does not call
//! it, and that gap is deliberate. See `tests/runtime_lifecycle.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use jatayu_core::id::{AccountId, DraftId, MailMessageId, ThreadId};
use jatayu_core::mail::rate_limit::RateLimitRefusal;
use jatayu_core::mail::voice::VoiceProfile;
use jatayu_core::mail::{RateLimitState, TokenBucket};
use jiff::{SignedDuration, Timestamp};

use crate::mail::sync::wiring::MailState;

/// A thread's suggestions: how many messages it had, when they were written, and
/// the suggestions.
type CachedReplies = (u32, Timestamp, Vec<crate::mail::write::ReplySuggestion>);

pub(crate) struct MailRuntime {
    /// Mail's pack store and search index, and each account's sync
    /// progress -- open and populated for exactly as long as the vault they
    /// belong to is unlocked. See `mail::sync::wiring` for how it is opened
    /// (with a key derived from the vault's own, never reused) and
    /// [`MailRuntime::set_state`] for the door `mail::sync::wiring` reaches it
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
    /// [`Origin::is_rate_limited`](jatayu_core::mail::Origin::is_rate_limited)
    /// says so, and [`MailRuntime::check_rate_limit`] returns before ever
    /// touching this map for either.
    rate_limits: Mutex<HashMap<String, RateLimitState>>,
    /// Paces `jatayu_service::mail::ai`'s background model-assisted
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
    /// As [`MailRuntime::categorize_budget`], for sorting on a decision
    /// model (`crate::mail::sort`) -- larger, because one of those answers is
    /// a fraction of a second and a few hundred tokens of input, and an
    /// asked-for sort of older mail would otherwise take all day.
    sort_budget: Mutex<TokenBucket>,
    /// Each account's asked-for sort of older mail, while it runs -- see
    /// [`crate::mail::sort::OlderSort`]. Session state: a restart forgets one
    /// half done, and asking again picks up where it stopped, since what is
    /// already sorted is skipped.
    older_sorts: Mutex<HashMap<AccountId, crate::mail::sort::OlderSort>>,
    /// When each account's recent mail was last read all the way back to
    /// the window -- see `crate::mail::sort::FULL_LOOK_EVERY` for why the
    /// ticks in between read only the top of the Inbox.
    sort_full_looks: Mutex<HashMap<AccountId, Timestamp>>,
    /// As [`MailRuntime::sort_full_looks`], for the follow-up pass
    /// (`crate::mail::followup`), which reads the Inbox and Sent back to its own
    /// window.
    follow_up_full_looks: Mutex<HashMap<AccountId, Timestamp>>,
    /// Threads the follow-up pass read and had to leave for later, per
    /// account -- see [`crate::mail::followup::SkipNote`]. Replaced on every
    /// full look, so it never holds more than the window.
    follow_up_skips: Mutex<HashMap<AccountId, HashMap<ThreadId, crate::mail::followup::SkipNote>>>,
    /// What the follow-up pass has done for each account since the app
    /// started -- see [`crate::mail::followup::Tally`] -- for Settings → Mail
    /// sorting to say whether it ever looked, and what it found. Session
    /// state, on the terms of the other follow-up state here; unlike the
    /// skip notes, cleared by [`MailRuntime::on_lock`], since its counts are
    /// about one vault's accounts.
    follow_up_tallies: Mutex<HashMap<AccountId, crate::mail::followup::Tally>>,
    /// Held across every read-change-write of the learned voice
    /// (`jatayu_core::mail::VoiceLearning`): `suggest_replies` adds what it
    /// offered while a learning pass may be replacing the notes, and neither
    /// may put back a copy read before the other wrote.
    voice_learning: Mutex<()>,
    /// When the unattended learning pass last looked for replies to learn
    /// from -- see `crate::mail::voicelearn`'s `LOOK_AGAIN`.
    voice_learn_looked: Mutex<Option<Timestamp>>,
    /// `summarize_thread`'s cache: a thread's summary, keyed by how many
    /// messages it had when it was written. A thread that has grown since
    /// -- a new message landed -- misses the cache and is summarised again;
    /// one that has not is answered instantly. In memory, not the vault:
    /// losing it on a restart costs one re-summarise, never data nothing
    /// else remembers, the same trade `notify` and the rest of this session
    /// state already make.
    summary_cache: Mutex<HashMap<ThreadId, (u32, String)>>,
    /// Where [`jatayu_service::mail::ai::categorize_tick`]'s next pass over
    /// an account's `Other` threads should start -- see the field this
    /// replaces, `Service::mail_categorize_cursor`, for the full account of
    /// why this has to advance every tick and wrap on an empty page.
    categorize_cursor: Mutex<HashMap<AccountId, String>>,
    /// As [`MailRuntime::categorize_cursor`], for the auto-draft pass over
    /// `Important` threads.
    autodraft_cursor: Mutex<HashMap<AccountId, String>>,
    /// As [`MailRuntime::categorize_cursor`], for the priority pass over
    /// `Important` threads -- `jatayu_service::mail::ai`'s question of
    /// which of them need the person soon.
    priority_cursor: Mutex<HashMap<AccountId, String>>,
    /// Each account's [`VoiceProfile`], and when it was read -- reading one
    /// decrypts dozens of sent messages, so `jatayu_service::mail::voice`
    /// keeps it for a while rather than paying that on every suggestion.
    voice_cache: Mutex<HashMap<AccountId, (Timestamp, Arc<VoiceProfile>)>>,
    /// `suggest_replies`' cache: a thread's three suggestions, keyed by how
    /// many messages it had when they were written, so reopening a thread does
    /// not ask again. Unlike `summary_cache`, also by when: a suggestion can
    /// offer a time, and a time goes stale without the thread changing at all.
    reply_cache: Mutex<HashMap<ThreadId, CachedReplies>>,
    /// Whether a thread's newest message waits on the person's reply, as the
    /// decision model answered `crate::mail::followup::waits_on_reply`, with
    /// the message it answered about -- for while follow-ups are off and the
    /// answer is not kept on the thread, so that reopening it does not ask
    /// again.
    reply_wanted: Mutex<HashMap<ThreadId, (MailMessageId, bool)>>,
    /// When asking that last failed, so that for a while threads are opened
    /// without waiting on a decision model that is not answering.
    reply_question_failed: Mutex<Option<Timestamp>>,
    /// New mail waiting for its threads to be sorted ahead of everything
    /// else, and whether something is sorting them -- see
    /// [`crate::service::Service::sort_new_mail`].
    new_mail: Mutex<NewMailQueue>,
    /// Threads being asked about on that lane right now, which the minute's
    /// ordinary sorting passes over rather than asking about twice.
    sorting_now: Mutex<HashSet<ThreadId>>,
}

/// [`MailRuntime::new_mail`].
#[derive(Default)]
struct NewMailQueue {
    /// Per account, oldest first.
    waiting: Vec<(AccountId, Vec<MailMessageId>)>,
    running: bool,
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
        self.sort_budget.lock().unwrap().reseed(now);
    }

    pub(crate) fn new(
        now: Timestamp,
        categorize_per_minute: u32,
        autodraft_per_minute: u32,
        sort_per_minute: u32,
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
            sort_budget: Mutex::new(TokenBucket::new(
                sort_per_minute,
                f64::from(sort_per_minute) / 60.0,
                now,
            )),
            older_sorts: Mutex::new(HashMap::new()),
            sort_full_looks: Mutex::new(HashMap::new()),
            follow_up_full_looks: Mutex::new(HashMap::new()),
            follow_up_skips: Mutex::new(HashMap::new()),
            follow_up_tallies: Mutex::new(HashMap::new()),
            voice_learning: Mutex::new(()),
            voice_learn_looked: Mutex::new(None),
            summary_cache: Mutex::new(HashMap::new()),
            categorize_cursor: Mutex::new(HashMap::new()),
            autodraft_cursor: Mutex::new(HashMap::new()),
            priority_cursor: Mutex::new(HashMap::new()),
            voice_cache: Mutex::new(HashMap::new()),
            reply_cache: Mutex::new(HashMap::new()),
            reply_wanted: Mutex::new(HashMap::new()),
            reply_question_failed: Mutex::new(None),
            new_mail: Mutex::new(NewMailQueue::default()),
            sorting_now: Mutex::new(HashSet::new()),
        }
    }

    // ---- new mail, sorted first --------------------------------------

    /// Queue `messages` of `account` to have their threads sorted, and say
    /// whether the caller must start something to sort them: `false` while
    /// something already is, which picks these up before it stops.
    pub(crate) fn queue_new_mail(&self, account: AccountId, messages: Vec<MailMessageId>) -> bool {
        let mut queue = self.new_mail.lock().unwrap();
        match queue.waiting.iter_mut().find(|(a, _)| *a == account) {
            Some((_, waiting)) => {
                for message in messages {
                    if !waiting.contains(&message) {
                        waiting.push(message);
                    }
                }
            }
            None => queue.waiting.push((account, messages)),
        }
        !std::mem::replace(&mut queue.running, true)
    }

    /// The next account's waiting mail, or `None` -- and then nothing is
    /// sorting it any more, decided under the same lock a caller of
    /// [`MailRuntime::queue_new_mail`] takes, so none is left behind.
    pub(crate) fn next_new_mail(&self) -> Option<(AccountId, Vec<MailMessageId>)> {
        let mut queue = self.new_mail.lock().unwrap();
        if queue.waiting.is_empty() {
            queue.running = false;
            None
        } else {
            Some(queue.waiting.remove(0))
        }
    }

    /// Whatever was sorting new mail stopped without finishing -- aborted
    /// when the vault locked. What it had not reached is left to the
    /// ordinary sorting.
    pub(crate) fn new_mail_stopped(&self) {
        let mut queue = self.new_mail.lock().unwrap();
        queue.waiting.clear();
        queue.running = false;
    }

    /// Mark `threads` as being asked about by one sorting pass, and answer the
    /// ones no other pass was already asking about.
    pub(crate) fn claim_sorts(&self, threads: &[ThreadId]) -> Vec<ThreadId> {
        let mut now = self.sorting_now.lock().unwrap();
        threads.iter().copied().filter(|t| now.insert(*t)).collect()
    }

    pub(crate) fn release_sorts(&self, threads: &[ThreadId]) {
        let mut now = self.sorting_now.lock().unwrap();
        for thread in threads {
            now.remove(thread);
        }
    }

    pub(crate) fn sorting_now(&self, thread: ThreadId) -> bool {
        self.sorting_now.lock().unwrap().contains(&thread)
    }

    // ---- the open state itself --------------------------------------

    pub(crate) fn packs(&self) -> Option<Arc<dyn jatayu_core::packstore::PackStore>> {
        self.state.read().unwrap().as_ref().map(|m| m.packs.clone())
    }

    pub(crate) fn index(&self) -> Option<Arc<dyn jatayu_core::MailSearch>> {
        self.state.read().unwrap().as_ref().map(|m| m.index.clone())
    }

    pub(crate) fn statuses(&self) -> Option<crate::mail::sync::status::StatusRegistry> {
        self.state.read().unwrap().as_ref().map(|m| m.statuses.clone())
    }

    pub(crate) fn unread_cache(&self) -> Option<Arc<crate::mail::sync::unread_cache::UnreadCache>> {
        self.state.read().unwrap().as_ref().map(|m| m.unread_cache.clone())
    }

    pub(crate) fn contacts(&self) -> Option<Arc<crate::mail::sync::contacts::ContactIndex>> {
        self.state.read().unwrap().as_ref().map(|m| m.contacts.clone())
    }

    /// Replace what [`MailRuntime::packs`], [`MailRuntime::index`] and
    /// [`MailRuntime::statuses`] answer -- `mail::sync::wiring`'s own door
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

    pub(crate) fn sort_take(&self, now: Timestamp, want: u32) -> u32 {
        take_tokens(&self.sort_budget, now, want)
    }

    pub(crate) fn older_sort(&self, account: AccountId) -> Option<crate::mail::sort::OlderSort> {
        self.older_sorts.lock().unwrap().get(&account).cloned()
    }

    pub(crate) fn older_sorts(&self) -> Vec<(AccountId, crate::mail::sort::OlderSort)> {
        self.older_sorts.lock().unwrap().iter().map(|(a, s)| (*a, s.clone())).collect()
    }

    /// Apply `f` to `account`'s older sort, but only if it is still the run
    /// numbered `run`: one stopped or restarted while a tick was asking about
    /// its threads is not written back over. `f` answering `false` ends it.
    pub(crate) fn update_older_sort(
        &self,
        account: AccountId,
        run: u64,
        f: impl FnOnce(&mut crate::mail::sort::OlderSort) -> bool,
    ) {
        let mut sorts = self.older_sorts.lock().unwrap();
        let Some(sort) = sorts.get_mut(&account).filter(|s| s.run == run) else { return };
        if !f(sort) {
            sorts.remove(&account);
        }
    }

    pub(crate) fn sort_full_look(&self, account: AccountId) -> Option<Timestamp> {
        self.sort_full_looks.lock().unwrap().get(&account).copied()
    }

    pub(crate) fn set_sort_full_look(&self, account: AccountId, at: Option<Timestamp>) {
        let mut looks = self.sort_full_looks.lock().unwrap();
        match at {
            Some(at) => {
                looks.insert(account, at);
            }
            None => {
                looks.remove(&account);
            }
        }
    }

    pub(crate) fn set_older_sort(
        &self,
        account: AccountId,
        sort: Option<crate::mail::sort::OlderSort>,
    ) {
        let mut sorts = self.older_sorts.lock().unwrap();
        match sort {
            Some(s) => {
                sorts.insert(account, s);
            }
            None => {
                sorts.remove(&account);
            }
        }
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

    pub(crate) fn priority_cursor(&self, account: AccountId) -> Option<String> {
        self.priority_cursor.lock().unwrap().get(&account).cloned()
    }

    pub(crate) fn set_priority_cursor(&self, account: AccountId, cursor: Option<String>) {
        set_cursor(&self.priority_cursor, account, cursor);
    }

    /// `account`'s cached profile, if one was read no longer than `max_age`
    /// before `now`.
    pub(crate) fn voice_cached(
        &self,
        account: AccountId,
        now: Timestamp,
        max_age: SignedDuration,
    ) -> Option<Arc<VoiceProfile>> {
        let cache = self.voice_cache.lock().unwrap();
        cache
            .get(&account)
            .filter(|(at, _)| now.duration_since(*at) <= max_age)
            .map(|(_, p)| p.clone())
    }

    pub(crate) fn voice_cache_put(
        &self,
        account: AccountId,
        now: Timestamp,
        profile: Arc<VoiceProfile>,
    ) {
        self.voice_cache.lock().unwrap().insert(account, (now, profile));
    }

    pub(crate) fn replies_cached(
        &self,
        thread: ThreadId,
        message_count: u32,
        oldest: Timestamp,
    ) -> Option<Vec<crate::mail::write::ReplySuggestion>> {
        let cache = self.reply_cache.lock().unwrap();
        cache
            .get(&thread)
            .filter(|(n, written, _)| *n == message_count && *written >= oldest)
            .map(|(_, _, r)| r.clone())
    }

    pub(crate) fn replies_cache_put(
        &self,
        thread: ThreadId,
        message_count: u32,
        now: Timestamp,
        replies: Vec<crate::mail::write::ReplySuggestion>,
    ) {
        self.reply_cache.lock().unwrap().insert(thread, (message_count, now, replies));
    }

    pub(crate) fn reply_wanted(&self, thread: ThreadId, message: MailMessageId) -> Option<bool> {
        let known = self.reply_wanted.lock().unwrap();
        known.get(&thread).filter(|(about, _)| *about == message).map(|(_, waits)| *waits)
    }

    pub(crate) fn set_reply_wanted(&self, thread: ThreadId, message: MailMessageId, waits: bool) {
        self.reply_wanted.lock().unwrap().insert(thread, (message, waits));
    }

    pub(crate) fn reply_question_failed(&self) -> Option<Timestamp> {
        *self.reply_question_failed.lock().unwrap()
    }

    pub(crate) fn set_reply_question_failed(&self, at: Option<Timestamp>) {
        *self.reply_question_failed.lock().unwrap() = at;
    }

    pub(crate) fn voice_cache_forget(&self, account: AccountId) {
        self.voice_cache.lock().unwrap().remove(&account);
    }

    pub(crate) fn follow_up_full_look(&self, account: AccountId) -> Option<Timestamp> {
        self.follow_up_full_looks.lock().unwrap().get(&account).copied()
    }

    pub(crate) fn set_follow_up_full_look(&self, account: AccountId, at: Option<Timestamp>) {
        let mut looks = self.follow_up_full_looks.lock().unwrap();
        match at {
            Some(at) => {
                looks.insert(account, at);
            }
            None => {
                looks.remove(&account);
            }
        }
    }

    pub(crate) fn follow_up_skips(
        &self,
        account: AccountId,
    ) -> HashMap<ThreadId, crate::mail::followup::SkipNote> {
        self.follow_up_skips.lock().unwrap().get(&account).cloned().unwrap_or_default()
    }

    pub(crate) fn set_follow_up_skips(
        &self,
        account: AccountId,
        skips: HashMap<ThreadId, crate::mail::followup::SkipNote>,
    ) {
        self.follow_up_skips.lock().unwrap().insert(account, skips);
    }

    /// What the follow-up pass has done for `account` this session; nothing
    /// yet is the default tally.
    pub(crate) fn follow_up_tally(&self, account: AccountId) -> crate::mail::followup::Tally {
        self.follow_up_tallies.lock().unwrap().get(&account).cloned().unwrap_or_default()
    }

    pub(crate) fn set_follow_up_tally(
        &self,
        account: AccountId,
        tally: crate::mail::followup::Tally,
    ) {
        self.follow_up_tallies.lock().unwrap().insert(account, tally);
    }

    pub(crate) fn voice_learn_looked(&self) -> Option<Timestamp> {
        *self.voice_learn_looked.lock().unwrap()
    }

    pub(crate) fn set_voice_learn_looked(&self, at: Option<Timestamp>) {
        *self.voice_learn_looked.lock().unwrap() = at;
    }

    pub(crate) fn voice_learning_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.voice_learning.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn replies_cache_clear(&self) {
        self.reply_cache.lock().unwrap().clear();
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
    // `on_lock` bundles these, called in this order, right after
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

    fn forget_priority_cursor(&self) {
        self.priority_cursor.lock().unwrap().clear();
    }

    /// A sort of older mail is of this vault's mail.
    fn forget_older_sorts(&self) {
        self.older_sorts.lock().unwrap().clear();
        self.sort_full_looks.lock().unwrap().clear();
    }

    /// What the follow-up pass did is about this vault's accounts.
    fn forget_follow_up_tallies(&self) {
        self.follow_up_tallies.lock().unwrap().clear();
    }

    /// A profile, suggestions and whether a thread wants a reply are all read
    /// from this vault's mail, so none may outlive it being open.
    fn forget_writing_caches(&self) {
        self.voice_cache.lock().unwrap().clear();
        self.reply_cache.lock().unwrap().clear();
        self.reply_wanted.lock().unwrap().clear();
        *self.reply_question_failed.lock().unwrap() = None;
        *self.voice_learn_looked.lock().unwrap() = None;
    }

    /// Called once, by [`Service::close`](crate::service::Service::close),
    /// in place of the `forget_*` calls above written out by hand.
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
        self.forget_priority_cursor();
        self.forget_older_sorts();
        self.forget_follow_up_tallies();
        self.forget_writing_caches();
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

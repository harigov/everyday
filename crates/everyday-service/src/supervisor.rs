//! Long-lived tasks, kept alive.
//!
//! [`scheduler`](crate::scheduler) is one loop that wakes once a minute and
//! decides, each time, whether there is a routine to run. Nothing about it
//! holds a connection open, and nothing about it is keyed to anything --
//! there is exactly one scheduler for the whole vault, for ever, and it is
//! spawned once beside the process.
//!
//! Mail needs a different shape. An account with its inbox open wants one
//! task that stays up for as long as the vault does -- an IMAP `IDLE`
//! connection, reconnected when the network blinks, stopped the instant the
//! vault has no key to write new mail with and resumed the instant it does
//! again -- and it wants one of those *per account*, started and stopped as
//! accounts are added and removed rather than as the process starts and
//! stops. That is a different lifecycle from the scheduler's single
//! always-on loop, and different enough that bolting it onto `scheduler.rs`
//! would have made one file answer two questions.
//!
//! This is that different shape, built now and used by nothing yet. Phase 0
//! is groundwork: the supervisor exists, is tied to the vault's lock, and is
//! tested against everything the mail phase will actually ask of it, but it
//! starts no task of its own until an account exists to give it one.
//!
//! # What it promises a task
//!
//! * A key. [`Supervisor::ensure`] is idempotent on it: asking twice for
//!   `"account:1"` while it is already running, backing off, or even mid
//!   restart starts nothing a second time -- which is what lets a caller ask
//!   for every account's task on every unlock without first checking which
//!   ones happen to be up already. It is not, however, a pure no-op: an
//!   `ensure` that finds the key already running marks it to be run again
//!   once its current attempt finishes, even if that attempt was seconds
//!   from answering [`Outcome::Done`] when the call landed -- see
//!   `Entry::rerun_requested`'s own doc for the race this closes.
//! * A [`watch::Receiver<bool>`](tokio::sync::watch::Receiver), handed fresh
//!   to the factory on every attempt, that turns `true` the moment somebody
//!   asks this task to stop. A task that is only ever going to be aborted
//!   anyway does not strictly have to look at it -- see [`Supervisor::stop`]
//!   for why it is aborted regardless -- but a task that closes its socket
//!   and finishes cleanly when it sees the signal gets the chance to, rather
//!   than being cut off wherever it happened to be.
//! * A restart, with backoff, for a task whose future resolves to `Err` or
//!   which panics outright -- and *no* restart for one that resolves to
//!   `Ok(`[`Outcome::Done`]`)`, because that is the one answer that means
//!   "there is nothing more for this key to do", as distinct from "something
//!   went wrong, try again".
//! * Its own state, [`TaskState`], readable at any moment through
//!   [`Supervisor::state`] and announced as it changes through the same
//!   [`EventSink`] every other write in this crate announces through -- see
//!   [`events::Kind::BackgroundTask`](crate::events::Kind::BackgroundTask)
//!   for why that variant exists and what it is missing today.
//!
//! # What stopping means
//!
//! [`Supervisor::stop`] sends the watch signal, gives the task a short grace
//! period to notice it and return on its own, and aborts the task outright
//! if it has not. The grace period is cooperative manners, not a promise:
//! nothing here can force a future to await its own cancellation faster than
//! it chooses to poll, and the one thing every caller of `stop` actually
//! needs -- that it returns promptly whatever the task does -- is what the
//! forced abort guarantees. A vault's lock screen must not wait on an IMAP
//! server that has stopped answering.
//!
//! # Restart, backoff, and the vault's lock
//!
//! [`Supervisor::ensure`] both starts a task *and* remembers how to start it
//! again: the factory is kept, keyed the same way the task is, for as long
//! as nothing asks the supervisor to forget it. [`Supervisor::stop_all`]
//! stops every task that is running without forgetting any of them, which
//! is exactly the shape a locked vault needs -- a locked vault has no key
//! for a sync task to write with, so every one of them has to stop, but the
//! moment the vault is unlocked again the same accounts want the same tasks
//! back. [`Supervisor::restart_registered`] is that second half: it calls
//! `ensure` again for every key whose factory is still on file and whose
//! task is not currently running.
//!
//! `Service::locked` and the vault's `unlock` command are where this is
//! wired to the vault's own lock state -- see their doc comments in
//! `service.rs` and `domains/vault.rs`. Nothing there names mail; a
//! supervisor with nothing registered on it does nothing on either call,
//! which is exactly the "phase 0 wires the mechanism, phase 1 populates it"
//! shape the rest of this plan follows.

use crate::events::{Change, EventSink, Kind, Op};
use futures::FutureExt;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// How long [`Supervisor::stop`] waits for a task to return on its own,
/// having sent the signal, before it gives up and aborts it.
///
/// Short, on purpose: this is the number that stands between a person
/// pressing "lock" and the screen actually locking, and nobody should feel
/// an IMAP server's timeout through it. A task that wants longer to close
/// cleanly should do most of that closing *after* it notices the signal but
/// before it awaits anything slow -- flush what is already in memory first,
/// finish the network round trip second.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(5);

/// The smallest gap between a failed attempt and the next one.
const BACKOFF_BASE: Duration = Duration::from_secs(1);

/// The largest gap between attempts, however many have failed in a row.
///
/// Five minutes: long enough that a provider having a bad afternoon is not
/// hammered once a second for it, short enough that a fixed outage is
/// noticed again inside a coffee break.
const BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);

/// What a supervised task's future resolved to on its own, as opposed to
/// being stopped from outside.
///
/// An enum of one variant rather than a unit struct or a plain `Ok(())`,
/// because the entire point of this type is the question `ensure` and
/// `restart_registered` ask of it -- "does this key get to rest, or does it
/// get tried again" -- and a future second answer to that question (a task
/// that finished this attempt but wants to be woken at a particular time
/// rather than restarted immediately, say) has somewhere to go without
/// changing every match on this type that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The task's work is finished for good. It is not restarted, and its
    /// key is not tried again until something calls [`Supervisor::ensure`]
    /// for it afresh.
    Done,
}

/// What a task's future is answering `Err` with, on its way into
/// [`TaskState::Backoff`]'s `last_error`.
///
/// A boxed `std::error::Error` rather than a fixed enum, because this crate
/// has no idea what a mail sync's connection error, a calendar's HTTP
/// status, or a test's deliberate failure ought to share a type with -- the
/// supervisor's whole reason to exist is running tasks it does not
/// understand the insides of. It is converted to the `String` `last_error`
/// actually carries the moment it is caught, because a `Change` event has to
/// be `Send` and `'static` and cheap to clone, and nobody reads a stored
/// error's `source()` chain from a status line.
pub type TaskError = Box<dyn std::error::Error + Send + Sync>;

/// What a supervised task's one attempt answers with.
pub type TaskResult = Result<Outcome, TaskError>;

/// The future one attempt at a task is. Boxed because a `HashMap` of them
/// has to name one type for every key, whatever kind of work is actually
/// behind each.
pub type TaskFuture = BoxFuture<'static, TaskResult>;

/// What starts one attempt at a task, given the receiver that turns `true`
/// when this key is asked to stop.
///
/// Called more than once over a key's lifetime -- once per attempt, so a
/// task that fails gets a fresh future, a fresh chance to open whatever
/// connection it needs, on every restart. It is `Fn`, not `FnOnce` or
/// `FnMut`, for exactly that reason: the supervisor owns it for as long as
/// the key is registered and calls it again and again without ever needing
/// exclusive access.
pub type Factory = Arc<dyn Fn(watch::Receiver<bool>) -> TaskFuture + Send + Sync>;

/// One task's observable condition.
///
/// Mirrors nothing on the wire -- see [`Kind::BackgroundTask`]'s doc for
/// why a `Change` event only says *that* a key's state moved, never to
/// what -- so this has no `Serialize` impl and no camelCase obligations; it
/// exists for [`Supervisor::state`] and for the tests that exercise every
/// transition below.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
    /// Asked to run. Set the instant [`Supervisor::ensure`] or
    /// [`Supervisor::restart_registered`] decides to spawn it, before the
    /// runtime has necessarily polled it even once.
    Starting,
    /// Polling its factory's future.
    Running,
    /// The last attempt ended in `Err` or a panic, and this is waiting out
    /// the delay before the next one.
    Backoff { until: jiff::Timestamp, last_error: String },
    /// Not running, and not going to be restarted on its own: either
    /// [`Supervisor::stop`] asked it to, or its own future answered
    /// `Ok(Outcome::Done)`. [`Supervisor::restart_registered`] is what moves
    /// a key out of this state again, if its factory is still on file.
    Stopped,
}

/// One task's bookkeeping. Outlives any one attempt: a key whose task has
/// stopped keeps its `factory` and its `generation` so that a later
/// `ensure` or a `restart_registered` can start it again without whoever is
/// calling either having to have kept the factory themselves.
struct Entry {
    factory: Factory,
    state: TaskState,
    /// This key's *current* attempt's stop signal, and the join handle
    /// driving it -- both `None` exactly when [`Entry::state`] is
    /// [`TaskState::Stopped`].
    stop_tx: Option<watch::Sender<bool>>,
    handle: Option<JoinHandle<()>>,
    /// Incremented every time this key is (re)started. A [`set_state`] call
    /// from a task whose generation has been superseded -- because it was
    /// stopped and immediately re-`ensure`d while its old attempt was still
    /// unwinding towards the abort it had already been sent -- is a report
    /// about an attempt nobody cares about any more, and is dropped rather
    /// than allowed to clobber the state the newer attempt has already set.
    generation: u64,
    /// Set by [`Supervisor::start`] when `ensure` is called for this key
    /// while `handle` is still `Some` -- "please run again" arriving too
    /// late to be started as a fresh attempt, because one is already
    /// registered. Checked, and cleared, by [`drive`] the moment its
    /// current attempt resolves to [`Outcome::Done`]: if it is set, the
    /// `Done` just received is stale -- something wants this key looked at
    /// again -- so `drive` loops back to `Running` instead of finishing.
    ///
    /// Without this, a caller whose `ensure` lands in the narrow window
    /// between a task's future resolving to `Done` and [`finish`] clearing
    /// its `handle` (both guarded by the same `tasks` lock, but on two
    /// different sides of it) sees `handle.is_some()`, does nothing, and
    /// the request is simply lost -- the exact race `meeting::pipeline`'s
    /// `finish` landing while `chunk_closed`'s own early attempt is mid-exit
    /// can hit, leaving a recording stuck until something else re-`enqueue`s
    /// it.
    rerun_requested: bool,
}

struct Shared {
    events: Mutex<Arc<dyn EventSink>>,
    tasks: Mutex<HashMap<String, Entry>>,
}

/// Runs and restarts a set of keyed, long-lived tasks.
///
/// See the module doc for the shape this exists to serve. Cheap to clone --
/// it is one `Arc` -- so the same supervisor can be handed to whatever
/// constructs the scheduler beside it and to `Service` for the lock hook,
/// without either owning the other.
#[derive(Clone)]
pub struct Supervisor(Arc<Shared>);

impl Supervisor {
    /// A supervisor with nothing running on it, announcing through `events`.
    pub fn new(events: Arc<dyn EventSink>) -> Self {
        Self(Arc::new(Shared { events: Mutex::new(events), tasks: Mutex::new(HashMap::new()) }))
    }

    /// Point future announcements at a new sink.
    ///
    /// `Service::set_events` calls this every time it is called, which is
    /// more often than once: `everyday-app` composes a fresh fanout every
    /// time a paired device or an MCP client connects or disconnects (see
    /// `recompose_events` in `everyday-app/src/state.rs`), and a supervisor
    /// that had kept the sink it was constructed with would quietly stop
    /// reaching either the moment either first turned on. Kept in its own
    /// `Mutex` rather than folded into `tasks`' -- a task announcing its own
    /// state only ever needs to *read* this, never to hold both locks at
    /// once, so there is nothing for two separate locks to deadlock over.
    pub fn set_events(&self, events: Arc<dyn EventSink>) {
        *self.0.events.lock().unwrap() = events;
    }

    /// Start `key`'s task if it is not already running, backing off, or in
    /// the brief `Starting` window between being asked for and actually
    /// being polled.
    ///
    /// `factory` is kept as well as called: a later `stop` on this key does
    /// not forget it, so a subsequent `ensure` -- or a `restart_registered`
    /// after the vault unlocks -- can start the same task again without the
    /// caller having to hold on to the factory itself. Calling `ensure`
    /// again for a key that is already running does *not* replace its
    /// stored factory with this one; the running task was started with the
    /// factory it has, and swapping it under a task that has not stopped
    /// would be a surprise nothing here promises to protect against.
    pub fn ensure<F>(&self, key: impl Into<String>, factory: F)
    where
        F: Fn(watch::Receiver<bool>) -> TaskFuture + Send + Sync + 'static,
    {
        self.start(key.into(), Arc::new(factory));
    }

    fn start(&self, key: String, factory: Factory) {
        // Inserted *before* the task is spawned, and with the final
        // generation already decided, so that a driver which happens to run
        // its first poll before this function's second half runs (entirely
        // possible on a multi-threaded runtime) finds its own entry already
        // there to update rather than racing this function to create it.
        let generation = {
            let mut tasks = self.0.tasks.lock().unwrap();
            if let Some(entry) = tasks.get_mut(&key) {
                if entry.handle.is_some() {
                    // Already running (or between resolving and `finish`
                    // clearing its handle -- see `Entry::rerun_requested`'s
                    // own doc for that exact window). Ask it to run again
                    // once it is done, rather than starting a second
                    // attempt underneath it.
                    entry.rerun_requested = true;
                    return;
                }
            }
            let generation = tasks.get(&key).map_or(0, |e| e.generation.wrapping_add(1));
            tasks.insert(
                key.clone(),
                Entry {
                    factory: factory.clone(),
                    state: TaskState::Starting,
                    stop_tx: None,
                    handle: None,
                    generation,
                    rerun_requested: false,
                },
            );
            generation
        };
        self.0.events.lock().unwrap().changed(task_change(&key));

        let (stop_tx, stop_rx) = watch::channel(false);
        let shared = self.0.clone();
        let spawn_key = key.clone();
        let handle = tokio::spawn(drive(shared, spawn_key, factory, stop_rx, generation));

        let mut tasks = self.0.tasks.lock().unwrap();
        if let Some(entry) = tasks.get_mut(&key) {
            if entry.generation == generation {
                entry.stop_tx = Some(stop_tx);
                entry.handle = Some(handle);
            } else {
                // Superseded already -- stopped and re-`ensure`d between the
                // insert above and here. The handle just spawned belongs to
                // an attempt nobody wants; let it go.
                handle.abort();
            }
        }
    }

    /// Stop `key`'s task, if it has one running. Its factory stays on file,
    /// so a later `ensure` or `restart_registered` starts the same task
    /// again.
    ///
    /// Sends the stop signal, waits up to [`STOP_GRACE`] for the task to
    /// return on its own, and aborts it if it has not -- see the module doc
    /// on what that grace period is and is not a promise of.
    pub async fn stop(&self, key: &str) {
        let (stop_tx, handle) = {
            let mut tasks = self.0.tasks.lock().unwrap();
            let Some(entry) = tasks.get_mut(key) else { return };
            (entry.stop_tx.take(), entry.handle.take())
        };
        // Neither present means this key is already `Stopped` -- nothing to
        // do, and nothing worth telling anyone about a second time.
        if stop_tx.is_none() && handle.is_none() {
            return;
        }
        if let Some(tx) = stop_tx {
            let _ = tx.send(true);
        }
        if let Some(handle) = handle {
            let aborter = handle.abort_handle();
            if tokio::time::timeout(STOP_GRACE, handle).await.is_err() {
                aborter.abort();
            }
        }
        {
            let mut tasks = self.0.tasks.lock().unwrap();
            if let Some(entry) = tasks.get_mut(key) {
                entry.state = TaskState::Stopped;
            }
        }
        self.0.events.lock().unwrap().changed(task_change(key));
    }

    /// [`Supervisor::stop`] every task that is currently running, in
    /// parallel -- what a locked vault needs, and what `Service::locked`
    /// calls.
    ///
    /// Parallel rather than one after another, because `stop` on a task that
    /// ignores its signal costs up to [`STOP_GRACE`], and a vault with a
    /// dozen accounts open must not make the lock screen wait a dozen times
    /// that long for one of them to be slow.
    pub async fn stop_all(&self) {
        let keys: Vec<String> = {
            let tasks = self.0.tasks.lock().unwrap();
            tasks.iter().filter(|(_, e)| e.handle.is_some()).map(|(k, _)| k.clone()).collect()
        };
        futures::future::join_all(keys.iter().map(|key| self.stop(key))).await;
    }

    /// Start every registered key that is not currently running -- what an
    /// unlocked vault needs, and what the vault's `unlock` command calls.
    ///
    /// "Registered" means a factory is still on file for it, which is true
    /// for every key `stop` or `stop_all` has touched and false for one
    /// nothing has ever called [`Supervisor::ensure`] for. Phase 0 registers
    /// none, so this does nothing yet outside the tests below -- see the
    /// module doc.
    pub fn restart_registered(&self) {
        let to_start: Vec<(String, Factory)> = {
            let tasks = self.0.tasks.lock().unwrap();
            tasks
                .iter()
                .filter(|(_, e)| e.handle.is_none())
                .map(|(k, e)| (k.clone(), e.factory.clone()))
                .collect()
        };
        for (key, factory) in to_start {
            self.start(key, factory);
        }
    }

    /// `key`'s current state, or `None` if nothing has ever registered it.
    pub fn state(&self, key: &str) -> Option<TaskState> {
        self.0.tasks.lock().unwrap().get(key).map(|e| e.state.clone())
    }

    /// Every registered key and its current state, for a future status
    /// command to read -- nothing in phase 0 exposes this over the wire; see
    /// [`Kind::BackgroundTask`].
    pub fn states(&self) -> Vec<(String, TaskState)> {
        self.0.tasks.lock().unwrap().iter().map(|(k, e)| (k.clone(), e.state.clone())).collect()
    }
}

/// Move `key` to `state` and announce it, unless `generation` has already
/// been superseded by a newer attempt -- see [`Entry::generation`].
fn set_state(shared: &Shared, key: &str, generation: u64, state: TaskState) {
    {
        let mut tasks = shared.tasks.lock().unwrap();
        match tasks.get_mut(key) {
            Some(entry) if entry.generation == generation => entry.state = state,
            _ => return,
        }
    }
    shared.events.lock().unwrap().changed(task_change(key));
}

/// [`set_state`], and also clears `handle` and `stop_tx` -- what a task
/// ending *on its own* (returning [`Outcome::Done`]) has to do that
/// [`Supervisor::stop`] does not, because `stop` already takes both out of
/// the registry itself before this task even gets a chance to. A `handle`
/// left behind after the task it names has actually finished is what
/// [`Supervisor::start`]'s idempotency check would otherwise mistake for
/// still running -- see [`drive`]'s own call site.
fn finish(shared: &Shared, key: &str, generation: u64, state: TaskState) {
    {
        let mut tasks = shared.tasks.lock().unwrap();
        match tasks.get_mut(key) {
            Some(entry) if entry.generation == generation => {
                entry.handle = None;
                entry.stop_tx = None;
                entry.state = state;
            }
            _ => return,
        }
    }
    shared.events.lock().unwrap().changed(task_change(key));
}

/// Read and clear `key`'s [`Entry::rerun_requested`], for the same
/// `generation` [`drive`] is currently running -- see that field's own doc.
/// `false` if the entry is gone or has already moved on to a newer
/// generation, the same guard [`set_state`] and [`finish`] use.
fn take_rerun(shared: &Shared, key: &str, generation: u64) -> bool {
    let mut tasks = shared.tasks.lock().unwrap();
    match tasks.get_mut(key) {
        Some(entry) if entry.generation == generation => std::mem::take(&mut entry.rerun_requested),
        _ => false,
    }
}

fn task_change(key: &str) -> Change {
    let mut change = Change::new(Kind::BackgroundTask, Op::Updated);
    change.id = Some(key.to_string());
    change
}

/// One key's whole life for as long as it keeps being restarted: run an
/// attempt, and on anything but [`Outcome::Done`], wait out a backoff and
/// run another.
///
/// Spawned once per [`Supervisor::start`] and left running -- or backing
/// off -- until [`Supervisor::stop`] aborts it or the factory answers
/// `Ok(Outcome::Done)`. A panic inside the factory's own future is caught
/// inside [`run_once`] and treated exactly like an `Err`, which is what lets
/// a task that panics be restarted rather than simply vanishing: catching it
/// only at the `tokio::spawn` boundary, the way a fire-and-forget task
/// normally would, would isolate the panic from the rest of the process but
/// would leave this loop -- and therefore this key -- dead, with nothing to
/// notice and nothing to restart it.
async fn drive(
    shared: Arc<Shared>,
    key: String,
    factory: Factory,
    stop_rx: watch::Receiver<bool>,
    generation: u64,
) {
    let mut attempt: u32 = 0;
    loop {
        set_state(&shared, &key, generation, TaskState::Running);
        // `tokio::time::Instant`, not `std::time::Instant`: this has to
        // agree with `tokio::time::sleep`/`advance` under a paused clock
        // the way every timing test in this file already does, or the
        // "healthy run" check just below would never see one in a test at
        // all, no matter how much virtual time the test advances.
        let started = tokio::time::Instant::now();
        match run_once(&factory, stop_rx.clone()).await {
            Ok(Outcome::Done) => {
                // A concurrent `ensure` for this same key may have landed
                // between this attempt's future resolving and this line --
                // both `start`'s idempotency check and this arm read and
                // write through the same `tasks` lock, but on either side of
                // that gap, `start` still saw `handle.is_some()` and set
                // `Entry::rerun_requested` rather than starting a second
                // attempt (see that field's own doc). Honour it: this
                // `Done` is stale, so loop back to `Running` and call the
                // factory again instead of retiring the key.
                if take_rerun(&shared, &key, generation) {
                    continue;
                }
                // Unlike `Supervisor::stop`, nobody has taken this task's own
                // `handle` out of the registry on this path -- it finished on
                // its own, by returning rather than by being aborted. Without
                // clearing it here, `start`'s own `entry.handle.is_some()`
                // check (its idempotency guard against asking twice for a
                // task already running) would go on believing a task that
                // has actually ended is still up, forever -- `ensure`,
                // `restart_registered` and a later `sync_account` would all
                // silently do nothing.
                finish(&shared, &key, generation, TaskState::Stopped);
                return;
            }
            Err(last_error) => {
                // An attempt that ran healthily for at least `BACKOFF_CAP`
                // before failing gets a clean slate: without this, `attempt`
                // only ever climbs, so an account that reconnects cleanly
                // after ten unrelated blinks over a week is, by the
                // eleventh, drawing up to the full five-minute cap for a
                // blink that would otherwise have resolved in a second or
                // two. A healthy run this long is evidence the *previous*
                // run of failures is over, not a continuation of it.
                if started.elapsed() >= BACKOFF_CAP {
                    attempt = 0;
                }
                attempt += 1;
                let delay = backoff_delay(attempt);
                let until = jiff::Timestamp::now()
                    .checked_add(jiff::SignedDuration::from_millis(delay.as_millis() as i64))
                    .unwrap_or_else(|_| jiff::Timestamp::now());
                set_state(&shared, &key, generation, TaskState::Backoff { until, last_error });
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// One attempt, with a panic turned into the same `Err` a returned one would
/// be.
///
/// The message is read out of a panic hook rather than out of what
/// [`catch_unwind`](futures::FutureExt::catch_unwind) hands back, because
/// the two do not agree here: `PanicHookInfo::payload` reliably downcasts to
/// `&str` or `String` for an ordinary `panic!`, but the boxed payload
/// `catch_unwind` itself returns, once it has travelled back out through a
/// boxed trait-object future, does not always downcast to either on every
/// toolchain -- the hook is std's own extraction of the same information
/// and does not depend on what wrapped the future on the way there. See
/// [`capture_panic_messages`].
async fn run_once(factory: &Factory, stop_rx: watch::Receiver<bool>) -> Result<Outcome, String> {
    capture_panic_messages();
    let attempt = factory(stop_rx);
    match AssertUnwindSafe(attempt).catch_unwind().await {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(_) => {
            let message = LAST_PANIC_MESSAGE
                .with(|cell| cell.borrow_mut().take())
                .unwrap_or_else(|| "the task panicked".to_string());
            Err(message)
        }
    }
}

std::thread_local! {
    /// The message from this thread's most recent panic, if a supervised
    /// task's hook has caught one that nothing has read yet.
    ///
    /// Thread-local rather than a shared `Mutex`, because the hook runs on
    /// the panicking thread itself, before unwinding starts, and so does
    /// the [`run_once`] call that reads this straight back out -- there is
    /// no `.await` between the panic and the read, so the two are
    /// guaranteed to be the same poll of the same task on the same thread
    /// even on a multi-threaded runtime, and nothing here needs to be
    /// `Send`.
    static LAST_PANIC_MESSAGE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Chain a panic hook that remembers each panic's message, once per process.
///
/// The default hook already prints a panic to stderr, which is worth
/// keeping -- a task that is about to be transparently restarted should
/// still leave the usual trace behind for whoever is looking at the log --
/// so this wraps rather than replaces it. Installed lazily, on a supervised
/// task's first attempt, rather than by whoever constructs a [`Supervisor`]:
/// most processes that build one will never see a supervised task panic at
/// all, and a hook that runs on every panic in the whole process, including
/// ones nothing here will ever restart, is a cost worth paying only once
/// something here actually needs it.
fn capture_panic_messages() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let message = match info.payload().downcast_ref::<&str>() {
                Some(s) => (*s).to_string(),
                None => match info.payload().downcast_ref::<String>() {
                    Some(s) => s.clone(),
                    None => "the task panicked".to_string(),
                },
            };
            LAST_PANIC_MESSAGE.with(|cell| *cell.borrow_mut() = Some(message));
            previous(info);
        }));
    });
}

/// Capped exponential backoff with full jitter: a delay drawn uniformly from
/// `0..=min(BACKOFF_BASE * 2^(attempt - 1), BACKOFF_CAP)`.
///
/// Full jitter rather than a fixed delay or a narrower jittered band,
/// because the case this exists for is many of a vault's accounts failing
/// at once -- a network that just came back, a provider having a bad
/// afternoon -- and a supervisor that made every one of them wait exactly
/// the same length of time would have them all retry in the same instant
/// and fail together again. See Marc Brooker's "Exponential Backoff and
/// Jitter" (the AWS Architecture Blog, 2015) for the fuller argument; this
/// is its "FullJitter".
fn backoff_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(10);
    let scaled = BACKOFF_BASE.saturating_mul(1u32 << exponent);
    full_jitter(scaled.min(BACKOFF_CAP))
}

fn full_jitter(capped: Duration) -> Duration {
    let millis = u64::try_from(capped.as_millis()).unwrap_or(u64::MAX);
    if millis == 0 {
        return Duration::ZERO;
    }
    use rand::Rng;
    Duration::from_millis(rand::rng().random_range(0..=millis))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Silent;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Collects every `Change` raised, so a test can assert on the sequence
    /// of states a key passed through rather than only the last one.
    #[derive(Default)]
    struct Collector(Mutex<Vec<Change>>);

    impl EventSink for Collector {
        fn changed(&self, change: Change) {
            self.0.lock().unwrap().push(change);
        }
    }

    impl Collector {
        fn ids(&self) -> Vec<String> {
            self.0.lock().unwrap().iter().filter_map(|c| c.id.clone()).collect()
        }
    }

    /// Poll the runtime until `f` is true or `tries` yields have gone by,
    /// without needing virtual time to move.
    ///
    /// `#[tokio::test]` builds a current-thread runtime, so a spawned driver
    /// only gets to run *between* this test's own `.await` points -- there
    /// is no second thread quietly making progress underneath it. A single
    /// `yield_now` is enough after a synchronous `ensure`, but the paused
    /// timer used for backoff can need a couple more turns of the loop
    /// before `tokio::time::advance` has actually woken the sleeper and let
    /// it record its next state, so this polls rather than yielding once.
    async fn settle(f: impl Fn() -> bool) {
        for _ in 0..50 {
            if f() {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert!(f(), "did not settle");
    }

    fn boxed_err(msg: &str) -> TaskError {
        msg.to_string().into()
    }

    #[tokio::test(start_paused = true)]
    async fn restarts_after_an_error_with_backoff() {
        let events = Arc::new(Collector::default());
        let sup = Supervisor::new(events.clone());
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        sup.ensure("acct", move |_stop| {
            let attempts = counter.clone();
            Box::pin(async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n == 0 { Err(boxed_err("boom")) } else { Ok(Outcome::Done) }
            })
        });

        settle(|| matches!(sup.state("acct"), Some(TaskState::Backoff { .. }))).await;
        let Some(TaskState::Backoff { last_error, .. }) = sup.state("acct") else {
            panic!("expected Backoff");
        };
        assert_eq!(last_error, "boom");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // The backoff sleep is real time under a paused clock, so it needs
        // advancing before the second attempt runs.
        tokio::time::advance(BACKOFF_CAP).await;
        settle(|| matches!(sup.state("acct"), Some(TaskState::Stopped))).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(events.ids().contains(&"acct".to_string()));
    }

    /// Regression: `attempt` used to only ever climb, so an account back
    /// under `drive`'s own loop for a healthy stretch after several
    /// earlier blinks would still draw a near-`BACKOFF_CAP` delay for its
    /// next failure -- exactly as long a wait as if none of the time in
    /// between had ever passed. Eleven quick failures saturate
    /// `backoff_delay`'s own exponent; the twelfth attempt then runs
    /// healthily for longer than `BACKOFF_CAP` before failing too, and the
    /// delay drawn for *that* failure must look like attempt one's again,
    /// not attempt twelve's.
    #[tokio::test(start_paused = true)]
    async fn a_healthy_run_resets_the_backoff_counter() {
        let sup = Supervisor::new(Arc::new(Silent));
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        sup.ensure("acct", move |_stop| {
            let attempts = counter.clone();
            Box::pin(async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n < 11 {
                    Err(boxed_err("blink"))
                } else if n == 11 {
                    tokio::time::sleep(BACKOFF_CAP + Duration::from_secs(1)).await;
                    Err(boxed_err("a blink after a long healthy run"))
                } else {
                    Ok(Outcome::Done)
                }
            })
        });

        // Driven off `attempts` itself, not off `sup.state`'s own
        // `Backoff` variant: two calls in a row both land in `Backoff`
        // with nothing else distinguishing them at a glance, so settling
        // on "is it `Backoff` yet" can observe the *same* stale answer
        // twice -- once genuinely after a failure, once again before the
        // next `advance` has had any chance to matter -- and silently
        // skip an attempt. The attempt counter has no such ambiguity: it
        // only ever goes up, once per call.
        for expected in 1..=11 {
            settle(|| attempts.load(Ordering::SeqCst) >= expected).await;
            tokio::time::advance(BACKOFF_CAP).await;
        }
        // Attempt twelve (n == 11) is now in flight, the one that actually
        // awaits something (its own long sleep) -- so, unlike the eleven
        // above, `Running` is a state this test can reliably catch before
        // advancing the clock past it.
        settle(|| attempts.load(Ordering::SeqCst) >= 12).await;
        settle(|| matches!(sup.state("acct"), Some(TaskState::Running))).await;
        // The sleep this attempt awaits is `BACKOFF_CAP`-sized (five
        // minutes) -- long enough that a single `advance` call does not
        // reliably carry tokio's own paused-clock timer wheel past it in
        // one step, the coarser a wheel level gets the further out a
        // timer's deadline is. Chunked, breaking the moment this attempt's
        // own failure lands, rather than one large `advance` that risks
        // also carrying the *next* attempt's own backoff past this test's
        // own read of `until` below.
        for _ in 0..12 {
            tokio::time::advance(BACKOFF_CAP).await;
            if matches!(sup.state("acct"), Some(TaskState::Backoff { .. })) {
                break;
            }
        }
        let before = jiff::Timestamp::now();
        let Some(TaskState::Backoff { until, .. }) = sup.state("acct") else {
            panic!("expected Backoff after attempt twelve's own failure: {:?}", sup.state("acct"));
        };
        let delay = before.duration_until(until);
        // Unreset, attempt twelve's own exponent is already saturated at
        // `BACKOFF_CAP` (five minutes); reset, it is attempt one's, capped
        // at `BACKOFF_BASE` (one second). A little slack for the assertion
        // itself, nowhere near enough to also pass for the unreset value.
        assert!(
            delay <= jiff::SignedDuration::from_millis(1_500),
            "a healthy run must reset the backoff counter, not draw a near-cap delay: {delay:?}"
        );
    }

    #[tokio::test]
    async fn does_not_restart_after_done() {
        let sup = Supervisor::new(Arc::new(Silent));
        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        sup.ensure("acct", move |_stop| {
            let calls = counter.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Outcome::Done)
            })
        });

        settle(|| matches!(sup.state("acct"), Some(TaskState::Stopped))).await;
        // Give a wrongly-restarting driver a fair chance to have done so.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// The regression for the bug `finish` fixes: a task that finished on
    /// its own (`Outcome::Done`, not `Supervisor::stop`) left its `handle`
    /// behind in the registry, which made `start`'s idempotency check believe
    /// it was still running forever after -- so `ensure`, called again for
    /// the same key once whatever stopped it the first time has changed
    /// (mail: the account signing back in after `NeedsSignIn`, which is
    /// exactly what `sync_account` calls `ensure_account_task` -- itself
    /// `Supervisor::ensure` -- to do), must actually start a fresh attempt
    /// rather than silently doing nothing.
    #[tokio::test]
    async fn ensure_after_done_starts_a_fresh_attempt() {
        let sup = Supervisor::new(Arc::new(Silent));
        let calls = Arc::new(AtomicU32::new(0));

        let start = |sup: &Supervisor, calls: Arc<AtomicU32>| {
            sup.ensure("acct", move |_stop| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Outcome::Done)
                })
            });
        };

        start(&sup, calls.clone());
        settle(|| matches!(sup.state("acct"), Some(TaskState::Stopped))).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        start(&sup, calls.clone());
        settle(|| calls.load(Ordering::SeqCst) == 2).await;
        settle(|| matches!(sup.state("acct"), Some(TaskState::Stopped))).await;
    }

    /// The narrower race `Entry::rerun_requested` closes, one level up from
    /// `ensure_after_done_starts_a_fresh_attempt`: an `ensure` landing while
    /// the *current* attempt is still mid-flight, on its way to answering
    /// `Outcome::Done`, rather than after `finish` has already cleared its
    /// `handle`. Before this fix, `start`'s idempotency check saw
    /// `handle.is_some()` (the attempt has not returned yet, from the
    /// registry's point of view) and did nothing at all -- exactly the shape
    /// of `meeting::pipeline::finish`'s own `enqueue` landing while
    /// `chunk_closed`'s early attempt was mid-exit, silently dropped.
    ///
    /// The factory blocks on a gate for its first call only, so the test
    /// controls precisely when "about to answer `Done`" happens: the second
    /// `ensure` below is made, and observed to have set the registry's own
    /// bookkeeping, strictly before the gate is opened and the first
    /// attempt is allowed to resolve.
    #[tokio::test]
    async fn ensure_while_a_task_is_about_to_finish_asks_it_to_run_again() {
        let sup = Supervisor::new(Arc::new(Silent));
        let calls = Arc::new(AtomicU32::new(0));
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let gate_rx = Arc::new(Mutex::new(Some(gate_rx)));

        let counter = calls.clone();
        let gate = gate_rx.clone();
        sup.ensure("acct", move |_stop| {
            let calls = counter.clone();
            let gate = gate.clone();
            Box::pin(async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // Parked here until the test releases it -- standing in
                    // for the gap between this future resolving and
                    // `finish` clearing `handle`, during which a concurrent
                    // `ensure` still finds `handle.is_some()`.
                    let rx = gate.lock().unwrap().take().unwrap();
                    let _ = rx.await;
                }
                Ok(Outcome::Done)
            })
        });
        settle(|| matches!(sup.state("acct"), Some(TaskState::Running))).await;

        // The factory given here is never used -- a key already running
        // keeps the one it started with (see `Supervisor::ensure`'s own
        // doc) -- only the request to run again matters.
        sup.ensure("acct", |_stop| Box::pin(async move { Ok(Outcome::Done) }));

        let _ = gate_tx.send(());

        settle(|| calls.load(Ordering::SeqCst) >= 2).await;
        settle(|| matches!(sup.state("acct"), Some(TaskState::Stopped))).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the ensure that landed mid-flight must still cause a second attempt, not be lost"
        );
    }

    #[tokio::test]
    async fn stop_all_stops_a_running_task() {
        let sup = Supervisor::new(Arc::new(Silent));
        // Never resolves on its own -- a stand-in for an IMAP `IDLE` loop
        // that only ever ends because it was asked to.
        sup.ensure("acct", |mut stop| {
            Box::pin(async move {
                loop {
                    if *stop.borrow() {
                        return Ok(Outcome::Done);
                    }
                    let _ = stop.changed().await;
                }
            })
        });
        settle(|| matches!(sup.state("acct"), Some(TaskState::Running))).await;

        sup.stop_all().await;
        assert_eq!(sup.state("acct"), Some(TaskState::Stopped));
    }

    #[tokio::test]
    async fn restart_registered_starts_what_stop_all_stopped() {
        let sup = Supervisor::new(Arc::new(Silent));
        let runs = Arc::new(AtomicU32::new(0));
        let counter = runs.clone();
        sup.ensure("acct", move |mut stop| {
            let runs = counter.clone();
            Box::pin(async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let _ = stop.changed().await;
                Ok(Outcome::Done)
            })
        });
        settle(|| runs.load(Ordering::SeqCst) == 1).await;

        sup.stop_all().await;
        assert_eq!(sup.state("acct"), Some(TaskState::Stopped));

        sup.restart_registered();
        settle(|| runs.load(Ordering::SeqCst) == 2).await;
        assert!(matches!(sup.state("acct"), Some(TaskState::Running)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_panic_is_a_restart_not_a_crash() {
        let sup = Supervisor::new(Arc::new(Silent));
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        sup.ensure("acct", move |_stop| {
            let attempts = counter.clone();
            Box::pin(async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    panic!("unexpected end of stream");
                }
                Ok(Outcome::Done)
            })
        });

        settle(|| matches!(sup.state("acct"), Some(TaskState::Backoff { .. }))).await;
        let Some(TaskState::Backoff { last_error, .. }) = sup.state("acct") else {
            panic!("expected Backoff after the panic");
        };
        assert_eq!(last_error, "unexpected end of stream");

        tokio::time::advance(BACKOFF_CAP).await;
        settle(|| matches!(sup.state("acct"), Some(TaskState::Stopped))).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_all_completes_promptly_even_if_a_task_ignores_the_signal() {
        let sup = Supervisor::new(Arc::new(Silent));
        // Deaf to `stop`: awaits only a timer far longer than the grace
        // period, so this only passes if `stop_all` aborts it once that
        // grace period is up rather than waiting for it to notice.
        sup.ensure("deaf", |_stop| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(Outcome::Done)
            })
        });
        settle(|| matches!(sup.state("deaf"), Some(TaskState::Running))).await;

        // `stop_all` is spawned rather than awaited directly, because it is
        // itself waiting on a paused timer -- `STOP_GRACE` -- and nothing
        // advances a paused clock while the only task on the runtime is the
        // one blocked waiting for it to move.
        let sup2 = sup.clone();
        let stopping = tokio::spawn(async move { sup2.stop_all().await });
        tokio::time::advance(STOP_GRACE).await;
        stopping.await.expect("stop_all's task panicked");

        assert_eq!(sup.state("deaf"), Some(TaskState::Stopped));
    }

    #[tokio::test]
    async fn run_once_turns_a_panic_into_its_message() {
        let (_tx, rx) = watch::channel(false);
        let factory: Factory = Arc::new(|_stop| Box::pin(async move { panic!("isolated") }));
        let result = run_once(&factory, rx).await;
        assert_eq!(result, Err("isolated".to_string()));
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert!(backoff_delay(1) <= BACKOFF_BASE);
        assert!(backoff_delay(20) <= BACKOFF_CAP);
    }

    /// Phase 9.3's pinning step: [`backoff_delay`]'s exact invariants --
    /// full jitter means the draw itself is not reproducible, but the
    /// *bound* each attempt draws from is, and so is how that bound grows
    /// -- fixed here before `backoff_delay` is rewritten to go through a
    /// shared `RetryPolicy`.
    #[test]
    fn backoff_delay_bounds_and_cap_growth_are_pinned() {
        let mut prev_cap = Duration::ZERO;
        for attempt in 1..=25u32 {
            let exponent = attempt.saturating_sub(1).min(10);
            let expected_cap = BACKOFF_BASE.saturating_mul(1u32 << exponent).min(BACKOFF_CAP);
            // Full jitter draws uniformly from `0..=expected_cap`; drawing
            // several times catches an off-by-one in either bound without
            // this test itself becoming a coin flip.
            for _ in 0..20 {
                let d = backoff_delay(attempt);
                assert!(d <= expected_cap, "attempt {attempt}: {d:?} exceeds cap {expected_cap:?}");
            }
            if attempt <= 11 {
                assert!(
                    expected_cap >= prev_cap,
                    "the cap must grow with attempt until it saturates at BACKOFF_CAP"
                );
            } else {
                assert_eq!(
                    expected_cap, BACKOFF_CAP,
                    "the cap must have saturated at BACKOFF_CAP by attempt 12"
                );
            }
            prev_cap = expected_cap;
        }
        // Never retries forever without a cap: even a huge attempt count
        // stays within BACKOFF_CAP, on every draw.
        for _ in 0..20 {
            assert!(backoff_delay(1_000_000) <= BACKOFF_CAP);
        }
    }

    /// The supervisor's give-up point, pinned: there is none. A restart
    /// loop only ever stops on `Outcome::Done` or `Supervisor::stop` --
    /// `does_not_restart_after_done` and `stop_all_stops_a_running_task`
    /// above already cover both; this is the negative case, that a purely
    /// failing task is never abandoned on its own.
    #[test]
    fn backoff_delay_never_refuses_to_answer_however_many_attempts_have_failed() {
        for attempt in [1, 2, 100, 10_000, u32::MAX] {
            let _ = backoff_delay(attempt); // must not panic or overflow
        }
    }
}

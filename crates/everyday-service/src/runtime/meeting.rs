//! The meeting watcher's and the spool's session state.
//!
//! Unlike [`crate::runtime::mail::MailRuntime`] and
//! [`crate::runtime::routine::RoutineRuntime`], nothing here is cleared
//! when the vault locks or closes -- see [`MeetingRuntime::on_lock`]'s own
//! doc for why, and `tests/runtime_lifecycle.rs`'s
//! `close_and_locked_never_clear_meeting_session_state` for the test that
//! pins it.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use everyday_core::id::{CalendarId, RecordingId};

pub(crate) struct MeetingRuntime {
    /// Calendar events the meeting watcher has already raised an offer for
    /// this session, keyed by calendar and the event's own uid (not its
    /// `EventId`, which a feed calendar mints fresh on every sync -- see
    /// [`everyday_core::meeting::EventRef::series_key`]'s neighbour,
    /// `Event::uid`, for why that is the durable half). Also where
    /// `dismiss_meeting_offer` records "not now": either way, the same call
    /// is not offered again until this process restarts or the vault
    /// relocks and unlocks -- `everyday_service::meeting::watch` is the
    /// only reader and writer.
    offered: RwLock<HashSet<(CalendarId, String)>>,
    /// When each recording last had a chunk appended to it, in this
    /// process. What `everyday_service::meeting::spool`'s unlock recovery
    /// reads to tell a call still being captured from one a crash or a quit
    /// left stuck in `Stage::Recording` -- a recording with no entry here
    /// has not been appended to since this process started, which after a
    /// restart is every recording still open, so recovery treats "no entry"
    /// the same as "stale". Session state on the same terms every other
    /// scheduler bookkeeping field here already is: losing it costs
    /// recovery nothing but immediacy, since a truly live capture keeps
    /// refreshing its own entry.
    last_append: Mutex<HashMap<RecordingId, Instant>>,
    /// Failed recordings the minute tick's expiry sweep has already raised
    /// its "will be deleted tomorrow" notification for, so it says so once
    /// per process rather than once an hour for as long as the recording
    /// sits in its last day. Cleared implicitly by never being consulted
    /// again once the recording is actually deleted or retried.
    expiry_warned: RwLock<HashSet<RecordingId>>,
}

impl Default for MeetingRuntime {
    fn default() -> Self {
        Self {
            offered: RwLock::new(HashSet::new()),
            last_append: Mutex::new(HashMap::new()),
            expiry_warned: RwLock::new(HashSet::new()),
        }
    }
}

impl MeetingRuntime {
    /// Mark `(calendar, uid)` as offered (or dismissed) this session, and
    /// say whether it was new -- `true` the first time, `false` on every
    /// later ask.
    pub(crate) fn offer_seen(&self, calendar: CalendarId, uid: &str) -> bool {
        self.offered.write().unwrap().insert((calendar, uid.to_string()))
    }

    /// Record that `id` had a chunk appended just now, in this process.
    /// `now` comes from the caller rather than `Instant::now()` here, so
    /// this stays reachable through `Service`'s own clock the way every
    /// other "now" in this crate is -- see [`crate::clock`]'s module doc.
    pub(crate) fn touch_append(&self, id: RecordingId, now: Instant) {
        self.last_append.lock().unwrap().insert(id, now);
    }

    /// How long ago `id` last had a chunk appended, in this process -- or
    /// `None` if it never has been. Reads a stored [`Instant`]'s own
    /// `elapsed()` rather than going through a clock, on the same
    /// reasoning `Service::instant`'s own doc gives for why this crate has
    /// no fake monotonic clock yet: nothing here is ever compared across a
    /// restart, only measured against the moment this call runs.
    pub(crate) fn since_append(&self, id: RecordingId) -> Option<Duration> {
        self.last_append.lock().unwrap().get(&id).map(Instant::elapsed)
    }

    /// Forget `id`'s append time -- called once a recording leaves
    /// `Stage::Recording`, so a long-finished call's id does not sit in
    /// this map for the rest of the session.
    pub(crate) fn forget_append(&self, id: RecordingId) {
        self.last_append.lock().unwrap().remove(&id);
    }

    /// Mark `id` as warned about its coming deletion, and say whether this
    /// was the first time -- `true` the first call, `false` after.
    pub(crate) fn expiry_warn_once(&self, id: RecordingId) -> bool {
        self.expiry_warned.write().unwrap().insert(id)
    }

    /// Deliberately empty. `close()` clears `MailRuntime`'s and
    /// `RoutineRuntime`'s session bookkeeping through their own `on_lock`;
    /// this session's meeting bookkeeping is not part of that today, on
    /// purpose -- "not now" should not have to be said again just because
    /// the screen relocked, and unlock recovery already treats a missing
    /// `last_append` entry the same as a stale one, which is what a real
    /// process restart (as opposed to a lock) leaves behind anyway. Kept as
    /// an explicit no-op, and called from the same place in `close()` the
    /// other two runtimes' `on_lock` is, so that this is a decision visible
    /// in `close()`'s own body rather than an absence a reader has to
    /// notice on their own.
    pub(crate) fn on_lock(&self) {}
}

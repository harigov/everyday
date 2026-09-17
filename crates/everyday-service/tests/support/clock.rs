//! A [`Clock`] a test moves by hand, so a time-dependent service
//! behaviour -- a proposal's expiry, a routine's due check -- can be proven
//! without an actual sleep and without backdating the stored record itself.
//!
//! Deliberately not `tokio::time::pause`/`advance`: those move a *different*
//! clock, the one `tokio::time::sleep`/`timeout` read, which has never
//! advanced `jiff::Timestamp::now()` -- see `everyday_service::clock`'s own
//! module doc. This is the wall clock `Service::now()` reads instead.

use std::sync::Mutex;
use std::time::Instant;

use everyday_service::clock::Clock;
use jiff::Timestamp;

pub struct FakeClock {
    now: Mutex<Timestamp>,
}

impl FakeClock {
    pub fn at(now: Timestamp) -> Self {
        Self { now: Mutex::new(now) }
    }

    /// Move this clock's `now()` to `at`, forward or back.
    pub fn set(&self, at: Timestamp) {
        *self.now.lock().unwrap() = at;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap()
    }

    // Nothing in this crate's tests needs a fake monotonic clock yet --
    // `Service::instant()`'s callers all measure an in-process duration
    // that a test proves some other way (a counted call, a channel).
    // Real time here, rather than a second thing to fake, until a test
    // actually needs it to move.
    fn instant(&self) -> Instant {
        Instant::now()
    }
}

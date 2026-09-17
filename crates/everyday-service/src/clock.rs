//! "Now", as one call [`Service`](crate::service::Service) hands to
//! everything that used to ask the operating system directly.
//!
//! Every write that stamps a timestamp, backs off a retry, or checks whether
//! something is due, used to call `jiff::Timestamp::now()` or
//! `std::time::Instant::now()` straight from wherever it sat. That makes the
//! call site untestable without an actual sleep -- a routine due in five
//! minutes, or a proposal that expires in a day, could only be proven by
//! waiting five minutes or a day. A `Service` now holds one clock, reachable
//! through [`Service::now`](crate::service::Service::now) and
//! [`Service::instant`](crate::service::Service::instant), and a test can
//! swap [`SystemClock`] for a [`FakeClock`] it moves by hand.
//!
//! # This is not tokio's clock
//!
//! `tokio::time::pause`/`advance`, which several tests already use to skip a
//! `tokio::time::sleep` without waiting for it, is a *separate* virtual clock
//! that only `tokio::time::Instant` and `tokio::time::sleep`/`timeout` read.
//! It has never advanced `jiff::Timestamp::now()` or `std::time::Instant::now()`,
//! and [`SystemClock`] keeps that exactly as it was: it wraps the real wall
//! clock and the real monotonic clock, neither of which tokio's pause
//! touches. A test that pauses tokio time to skip a sleep still sees the real
//! wall clock through `Service::now()`, precisely as it did before this
//! module existed -- and a test that wants a fixed or moving wall clock
//! instead reaches for [`FakeClock`], not for `tokio::time::pause`.
use std::sync::Arc;
use std::time::Instant;

use jiff::Timestamp;

/// A source of "now" for [`Service`](crate::service::Service). See the
/// module docs for why this is not tokio's clock.
pub trait Clock: Send + Sync {
    /// Wall-clock time: what a stored `updated_at`, a retry's `not_before`,
    /// or an expiry is measured against.
    fn now(&self) -> Timestamp;

    /// Monotonic time, for an in-process duration that is never written to
    /// the vault -- how long ago a chunk was appended, say -- and so is
    /// never compared across a restart.
    fn instant(&self) -> Instant;
}

/// The real clock: `jiff::Timestamp::now()` and `std::time::Instant::now()`,
/// exactly what every call site this replaces used before. The default for
/// every [`Service`](crate::service::Service) outside a test.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn instant(&self) -> Instant {
        Instant::now()
    }
}

/// A boxed [`SystemClock`], for [`Service::new`](crate::service::Service::new)'s
/// default field value.
pub fn system() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

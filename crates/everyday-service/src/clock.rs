//! "Now", as one call [`Service`](crate::service::Service) hands to
//! everything that used to ask the operating system directly.
//!
//! Every write that stamps a timestamp, backs off a retry, or checks whether
//! something is due, used to call `jiff::Timestamp::now()` or
//! `std::time::Instant::now()` straight from wherever it sat. That makes the
//! call site untestable without an actual sleep -- a proposal that expires in
//! a day could only be proven to expire by waiting a day. A `Service` now
//! holds one clock, reachable through [`Service::now`](crate::service::Service::now) and
//! [`Service::instant`](crate::service::Service::instant), and a test can
//! swap [`SystemClock`] for a fake it moves by hand -- see
//! `tests/support/clock.rs`'s `FakeClock`, which implements this trait from
//! outside the crate, the way a real caller would.
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
//! instead reaches for a fake `Clock`, not for `tokio::time::pause`.
//!
//! # Not every `now()` in this crate
//!
//! `everyday_core::agent::AgentSettings::now()` -- what the scheduler's
//! routine due-check reads -- calls `jiff::Timestamp::now()` itself, inside
//! `everyday-core`. That crate's own `Timestamp::now()` calls are out of
//! scope for this clock (see `docs/plans/architecture-refactor.md`, phase
//! 9.2): a routine's due-check is therefore not made fake-clock-testable by
//! this module, only the call sites inside `everyday-service` itself are.
//!
//! # Comparisons that still straddle two clocks
//!
//! A handful of checks read this clock on one side and a timestamp stamped
//! by the real one on the other, because the stamping half lives in
//! `everyday-core` or in the meeting pipeline, which is deliberately not
//! wired to a `Service` at all. Under [`SystemClock`] the two halves are the
//! same clock and every one of these behaves exactly as it always has --
//! which is why they were left alone. They matter only to a test that swaps
//! in a fake, and a fake set far from real time will read them as nonsense
//! in one direction or the other:
//!
//! - `meeting::spool::expire_failed` ages a recording against
//!   `Recording::updated_at`, stamped by `meeting::pipeline::failure::fail`
//!   and friends. A fake clock ahead of real time makes a recording that
//!   failed seconds ago look days old, and the expiry *deletes* its audio.
//! - `mailsync::task::next_pending_wake` sleeps until an op's `not_before`,
//!   which `outbox` writes from this clock. A fake clock behind real time
//!   means the sleep returns at once while `due_ops` still says "not due",
//!   which is a hot loop running a real sync pass each time round.
//! - `domains::calendars`'s `is_due` compares against `last_synced_at`,
//!   which `Calendar::mark_synced` stamps in core: a fake clock ahead
//!   re-fetches every feed every tick, behind refreshes nothing ever.
//! - `domains::library::set_item_progress` takes its dates from
//!   `today_local()` while stamping `updated_at` from this clock.
//! - [`Clock::instant`] is honoured where a monotonic moment is *stored*
//!   (`runtime::meeting`'s append tracker) but the reads still use
//!   `Instant::elapsed`, so a fake monotonic clock would not be believed.
//!   `FakeClock::instant` returns the real `Instant::now()` today, which is
//!   what keeps that consistent.
//!
//! So: a fake clock is safe for a test that stays near real time (the
//! proposal-expiry test moves days, not years) and for anything that only
//! reads this clock on both sides. A test that needs one of the pairs above
//! should convert the stamping half first rather than work around it.
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

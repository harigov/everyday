//! Rate limiting for ops whose [`Origin`](super::records::Origin) is
//! `Assistant` or `Mcp`.
//!
//! The plan's risk table names the failure this exists to stop: "the
//! assistant floods the outbox... exceeding it is an error the model
//! reads." Two limits, both pure and both driven by a `now` the caller
//! supplies rather than by a clock this module reads itself — the same
//! reason every other pure function in this crate takes its instant as a
//! parameter: a limiter that cannot be driven by a test's own fake clock is
//! a limiter whose edges nobody actually checks.
//!
//! - **Per turn.** A hard cap on how many ops one model turn may enqueue,
//!   independent of time — a single over-eager turn should not be able to
//!   spend a whole minute's budget in one go.
//! - **Per minute.** A [`TokenBucket`], refilling continuously rather than
//!   resetting on the minute, so a caller a few ops over a boundary is
//!   refused smoothly rather than allowed to burst twice by timing a batch
//!   either side of `:00`.
//!
//! A person's own actions (`Origin::Person`) are never rate limited here —
//! see [`Origin::is_rate_limited`](super::records::Origin::is_rate_limited)
//! — because a person clicking archive fifty times is not the failure mode
//! this module exists for.

use jiff::Timestamp;

/// A continuously-refilling bucket of tokens, the standard shape for "no
/// more than N per unit time, smoothed rather than bucketed by a wall-clock
/// boundary."
#[derive(Debug, Clone, PartialEq)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    updated: Timestamp,
}

impl TokenBucket {
    /// A full bucket of `capacity` tokens, refilling at `refill_per_sec`,
    /// as of `now`.
    pub fn new(capacity: u32, refill_per_sec: f64, now: Timestamp) -> Self {
        Self {
            capacity: f64::from(capacity),
            tokens: f64::from(capacity),
            refill_per_sec,
            updated: now,
        }
    }

    /// Start again from `now`, full.
    ///
    /// For a caller that has replaced its notion of the clock underneath a
    /// bucket already seeded from the old one -- a test swapping in a fake
    /// clock, in the only case there is today. [`refill`](Self::refill)
    /// refuses to refill from a moment before `updated`, so without this a
    /// bucket seeded from the real clock and then read on a clock set in the
    /// past never refills again.
    pub fn reseed(&mut self, now: Timestamp) {
        self.tokens = self.capacity;
        self.updated = now;
    }

    fn refill(&mut self, now: Timestamp) {
        let elapsed_secs = now.as_second() as f64 - self.updated.as_second() as f64
            + (now.subsec_nanosecond() as f64 - self.updated.subsec_nanosecond() as f64) / 1e9;
        if elapsed_secs > 0.0 {
            self.tokens = (self.tokens + elapsed_secs * self.refill_per_sec).min(self.capacity);
        }
        // A `now` that goes backwards (a clock adjustment, or a test driving
        // instants out of order) must not hand out extra tokens; it simply
        // does not refill, and the stored instant does not move backwards
        // either, so a later, correctly-ordered call still measures from the
        // last instant that was actually trusted.
        if now > self.updated {
            self.updated = now;
        }
    }

    /// Take one token if one is available, refilling first. `true` means the
    /// caller may proceed; `false` means it must wait.
    pub fn try_take(&mut self, now: Timestamp) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Why [`RateLimitState::check`] refused a call — read by the tool layer to
/// build the error text a model actually sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitRefusal {
    /// This turn has already enqueued its limit of ops.
    PerTurn,
    /// This caller has enqueued its limit of ops for the last minute.
    PerMinute,
}

/// Per-caller state: one of these per `Assistant { conversation }` or
/// `Mcp { client }`, kept by whatever holds the outbox for the life of a
/// session — this type only says whether the next op is allowed, and
/// updates itself when it is.
#[derive(Debug, Clone)]
pub struct RateLimitState {
    per_turn_limit: u32,
    current_turn: Option<String>,
    turn_count: u32,
    per_minute_limit: u32,
    minute_bucket: TokenBucket,
}

impl RateLimitState {
    /// A limiter allowing `per_turn_limit` ops in any one turn and
    /// `per_minute_limit` per rolling minute, fresh as of `now`.
    pub fn new(per_turn_limit: u32, per_minute_limit: u32, now: Timestamp) -> Self {
        Self {
            per_turn_limit,
            current_turn: None,
            turn_count: 0,
            per_minute_limit,
            minute_bucket: TokenBucket::new(
                per_minute_limit,
                f64::from(per_minute_limit) / 60.0,
                now,
            ),
        }
    }

    /// May one more op be enqueued for `turn`, as of `now`? Records the
    /// attempt if it is allowed, so the very next call sees its effect —
    /// there is no separate "commit" step, on the same reasoning
    /// `TokenBucket::try_take` checks and spends in one call: a caller that
    /// checked without spending could enqueue more than it asked permission
    /// for by racing its own check.
    ///
    /// A new `turn` (any string different from the one last seen) resets the
    /// per-turn counter to zero; the per-minute bucket is never reset by a
    /// turn boundary, because the limit it enforces is about the caller, not
    /// about any one turn.
    pub fn check(&mut self, turn: &str, now: Timestamp) -> Result<(), RateLimitRefusal> {
        if self.current_turn.as_deref() != Some(turn) {
            self.current_turn = Some(turn.to_string());
            self.turn_count = 0;
        }
        if self.turn_count >= self.per_turn_limit {
            return Err(RateLimitRefusal::PerTurn);
        }
        if !self.minute_bucket.try_take(now) {
            return Err(RateLimitRefusal::PerMinute);
        }
        self.turn_count += 1;
        Ok(())
    }

    pub fn per_turn_limit(&self) -> u32 {
        self.per_turn_limit
    }

    pub fn per_minute_limit(&self) -> u32 {
        self.per_minute_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    fn t(seconds: i64) -> Timestamp {
        Timestamp::UNIX_EPOCH + SignedDuration::from_secs(seconds)
    }

    // ---- TokenBucket ------------------------------------------------------

    #[test]
    fn a_fresh_bucket_starts_full() {
        let mut b = TokenBucket::new(5, 1.0, t(0));
        for _ in 0..5 {
            assert!(b.try_take(t(0)));
        }
        assert!(!b.try_take(t(0)), "the sixth token was never there");
    }

    #[test]
    fn tokens_refill_over_time() {
        let mut b = TokenBucket::new(2, 1.0, t(0));
        assert!(b.try_take(t(0)));
        assert!(b.try_take(t(0)));
        assert!(!b.try_take(t(0)));
        // One second later, one token's worth of refill has happened.
        assert!(b.try_take(t(1)));
        assert!(!b.try_take(t(1)));
    }

    #[test]
    fn refill_never_exceeds_capacity() {
        let mut b = TokenBucket::new(3, 10.0, t(0));
        // A very long gap must not let the bucket overflow past capacity.
        assert!(b.try_take(t(10_000)));
        assert!(b.try_take(t(10_000)));
        assert!(b.try_take(t(10_000)));
        assert!(!b.try_take(t(10_000)), "capacity caps the refill");
    }

    #[test]
    fn a_clock_moving_backwards_grants_no_extra_tokens() {
        let mut b = TokenBucket::new(1, 1.0, t(100));
        assert!(b.try_take(t(100)));
        // A call with an earlier `now` (a clock adjustment, or a misbehaving
        // caller) must not be treated as elapsed time.
        assert!(!b.try_take(t(50)));
        // And a later, correctly-ordered call still measures from `t(100)`,
        // not from the out-of-order `t(50)`.
        assert!(b.try_take(t(101)));
    }

    // ---- RateLimitState -----------------------------------------------------

    #[test]
    fn the_per_turn_limit_refuses_within_one_turn_but_resets_on_the_next() {
        let mut s = RateLimitState::new(2, 1_000, t(0));
        assert!(s.check("turn-1", t(0)).is_ok());
        assert!(s.check("turn-1", t(0)).is_ok());
        assert_eq!(s.check("turn-1", t(0)), Err(RateLimitRefusal::PerTurn));

        // A new turn starts a fresh count.
        assert!(s.check("turn-2", t(0)).is_ok());
    }

    #[test]
    fn the_per_minute_limit_refuses_across_turns() {
        let mut s = RateLimitState::new(1_000, 2, t(0));
        assert!(s.check("turn-1", t(0)).is_ok());
        assert!(s.check("turn-2", t(0)).is_ok());
        // A third op, in a brand new turn, is still refused: the per-minute
        // budget is about the caller, not about any one turn.
        assert_eq!(s.check("turn-3", t(0)), Err(RateLimitRefusal::PerMinute));
    }

    #[test]
    fn the_per_minute_budget_recovers_as_time_passes() {
        let mut s = RateLimitState::new(1_000, 60, t(0)); // one token per second
        assert!(s.check("t", t(0)).is_ok());
        for _ in 0..59 {
            let _ = s.check("t", t(0));
        }
        assert_eq!(s.check("t", t(0)), Err(RateLimitRefusal::PerMinute));
        assert!(s.check("t", t(1)).is_ok(), "a second later, one more token has refilled");
    }

    #[test]
    fn a_person_is_never_checked_against_this_limiter() {
        // Nothing to test in code -- `Origin::Person.is_rate_limited()` is `false`,
        // and it is the caller's job never to construct a `RateLimitState`
        // check for one. This test exists so the claim in the module docs
        // is pinned somewhere a reader trips over it.
        assert!(!crate::mail::Origin::Person.is_rate_limited());
    }
}

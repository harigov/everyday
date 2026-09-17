//! One retry mechanism, not one retry policy.
//!
//! Phase 9.3 of `docs/plans/architecture-refactor.md`: this crate had five
//! separate places computing "how long before the next attempt" and three
//! separate places computing "is this error worth retrying at all" --
//! `supervisor::backoff_delay`, `outbox`'s use of
//! `everyday_core::mail::backoff_for_attempt`, `accountcal::short_backoff`
//! (shared by `google.rs` and `graph.rs`), and
//! `meeting::pipeline::retry::retry_transient_from` each reimplementing the
//! same idea -- a delay that grows with how many attempts have already
//! failed, up to a cap -- in its own words.
//!
//! [`RetryPolicy`] is that one idea, named: an `attempt -> Duration`
//! function plus how many attempts are allowed before giving up. It is
//! deliberately *not* a driver -- nothing here sleeps, loops, or decides
//! what counts as a transient error. The four schedules stay exactly where
//! they were shaped for their own caller (`supervisor::supervisor_backoff`,
//! `outbox::outbox_table`, `accountcal::calendar_after_retry_after`,
//! `meeting::pipeline::retry::pipeline_transient`), each now built from one
//! of the two constructors below instead of hand-rolling the arithmetic, and
//! each call site still drives its own loop -- a supervised task's restart
//! loop, a durable op's deferred `not_before`, and a synchronous HTTP retry
//! are different enough mechanisms that forcing them through one shared
//! driver would be the "automatic... deliberate behaviour" the plan's
//! second pass already ruled out. What moves here is only the schedule
//! math, so a delay sequence pinned in `tests` before this module existed
//! keeps meaning the same thing after.

use std::sync::Arc;
use std::time::Duration;

/// A delay schedule plus its give-up rule.
///
/// Cheap to construct and to clone (`delay` is one `Arc`), so a caller that
/// wants one per call -- `pipeline_transient`'s `base` varies per test, for
/// instance -- does not need to cache it.
#[derive(Clone)]
pub(crate) struct RetryPolicy {
    delay: Arc<dyn Fn(u32) -> Duration + Send + Sync>,
    /// Total attempts allowed before giving up, counting the first (not
    /// just the retries). `None` means this schedule never gives up on its
    /// own -- the supervisor restarts until `Outcome::Done` or
    /// `Supervisor::stop`, and an outbox op keeps its own `not_before`
    /// growing until it succeeds, is cancelled, or fails for a reason that
    /// was never retryable in the first place.
    max_attempts: Option<u32>,
}

impl RetryPolicy {
    /// A capped exponential schedule: `min(base * 2^(attempt-1), cap)`,
    /// `attempt` 1-indexed (the delay returned is what to wait before the
    /// attempt numbered `attempt`, given `attempt - 1` have already
    /// failed), with full jitter -- a delay drawn uniformly from `0..=` that
    /// same bound -- when `jitter` is set. See
    /// `supervisor::backoff_delay`'s own doc, and Marc Brooker's
    /// "Exponential Backoff and Jitter" (the AWS Architecture Blog, 2015),
    /// for why full jitter exists at all: it is the supervisor's own
    /// reason, not every caller's -- `accountcal` and `meeting::pipeline`
    /// both pass `jitter: false`, because stampeding is not their failure
    /// mode (see each constructor's own doc for why).
    pub(crate) fn exponential(
        base: Duration,
        cap: Duration,
        jitter: bool,
        max_attempts: Option<u32>,
    ) -> Self {
        Self {
            delay: Arc::new(move |attempt: u32| {
                // Capped well under any real `attempt` this crate ever
                // passes (the largest, `supervisor`'s, saturates at 10) --
                // guards a caller that got the give-up rule wrong from
                // overflowing the shift rather than simply hitting `cap`.
                let exponent = attempt.saturating_sub(1).min(20);
                let scaled = base.saturating_mul(1u32.checked_shl(exponent).unwrap_or(u32::MAX));
                let capped = scaled.min(cap);
                if jitter { full_jitter(capped) } else { capped }
            }),
            max_attempts,
        }
    }

    /// A schedule read out of an existing `attempt -> Duration` function --
    /// what lets a policy point at code that already exists (a table, or a
    /// hand-written formula another module owns) instead of this module
    /// re-deriving it. [`outbox_table`] uses this to wrap
    /// `everyday_core::mail::backoff_for_attempt` without copying its
    /// numbers here.
    pub(crate) fn from_fn(
        delay: impl Fn(u32) -> Duration + Send + Sync + 'static,
        max_attempts: Option<u32>,
    ) -> Self {
        Self { delay: Arc::new(delay), max_attempts }
    }

    pub(crate) fn delay_for(&self, attempt: u32) -> Duration {
        (self.delay)(attempt)
    }

    /// Whether attempt number `attempt` (1-indexed, counting the one that
    /// just failed) is the last one this policy allows.
    pub(crate) fn gives_up_after(&self, attempt: u32) -> bool {
        self.max_attempts.is_some_and(|max| attempt >= max)
    }

    /// The total attempts this policy allows before giving up, counting the
    /// first. Only used from tests today -- production code asks
    /// [`gives_up_after`](Self::gives_up_after) instead, which is the
    /// question a retry loop actually has -- so this stays test-only rather
    /// than sitting unread in every other build.
    #[cfg(test)]
    pub(crate) fn max_attempts(&self) -> Option<u32> {
        self.max_attempts
    }
}

/// Draw a delay uniformly from `0..=capped` -- the "FullJitter" from Marc
/// Brooker's "Exponential Backoff and Jitter" (the AWS Architecture Blog,
/// 2015), lifted out of `supervisor::backoff_delay` unchanged: many
/// accounts failing at the same instant (a network that just came back)
/// must not all retry in the same instant and fail together again.
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

    #[test]
    fn exponential_without_jitter_is_deterministic_and_matches_the_closed_form() {
        let policy =
            RetryPolicy::exponential(Duration::from_secs(1), Duration::from_secs(10), false, None);
        assert_eq!(policy.delay_for(1), Duration::from_secs(1));
        assert_eq!(policy.delay_for(2), Duration::from_secs(2));
        assert_eq!(policy.delay_for(3), Duration::from_secs(4));
        assert_eq!(policy.delay_for(4), Duration::from_secs(8));
        assert_eq!(policy.delay_for(5), Duration::from_secs(10), "capped");
        assert_eq!(policy.delay_for(100), Duration::from_secs(10), "stays capped");
    }

    #[test]
    fn exponential_with_jitter_stays_within_the_uncapped_bound() {
        let policy = RetryPolicy::exponential(
            Duration::from_millis(100),
            Duration::from_secs(5),
            true,
            None,
        );
        for attempt in 1..=10 {
            for _ in 0..50 {
                let d = policy.delay_for(attempt);
                assert!(d <= Duration::from_secs(5), "attempt {attempt}: {d:?}");
            }
        }
    }

    #[test]
    fn from_fn_reads_straight_through() {
        let policy = RetryPolicy::from_fn(|attempt| Duration::from_secs(u64::from(attempt)), None);
        assert_eq!(policy.delay_for(0), Duration::ZERO);
        assert_eq!(policy.delay_for(7), Duration::from_secs(7));
    }

    #[test]
    fn gives_up_after_honours_max_attempts() {
        let bounded = RetryPolicy::exponential(
            Duration::from_secs(1),
            Duration::from_secs(1),
            false,
            Some(3),
        );
        assert!(!bounded.gives_up_after(1));
        assert!(!bounded.gives_up_after(2));
        assert!(bounded.gives_up_after(3));
        assert!(bounded.gives_up_after(4));
        assert_eq!(bounded.max_attempts(), Some(3));

        let unbounded =
            RetryPolicy::exponential(Duration::from_secs(1), Duration::from_secs(1), false, None);
        assert!(!unbounded.gives_up_after(1_000_000));
        assert_eq!(unbounded.max_attempts(), None);
    }
}

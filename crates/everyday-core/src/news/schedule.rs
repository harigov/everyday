//! When to ask a source again.
//!
//! A pure function of what is known -- the source's state, what happened
//! this time, what the server said about caching, and the instant -- with
//! the instant and the jitter passed in, so the table of cases beside it can
//! say exactly what happens rather than roughly. The routine clock is built
//! the same way, for the same reason.
//!
//! The rules, in the order they apply:
//!
//! - After a success, 60 minutes by default. Once three or more recent
//!   publication times are known, half the median gap between them instead,
//!   but never less than the default and never more than
//!   [`STRETCH_MAX_MINUTES`]: a blog that posts monthly is asked four times a
//!   day rather than twenty-four, and a busy feed is never asked more often
//!   than hourly.
//! - After a failure, the larger of the last interval and the default,
//!   doubled once for every failure in a row including this one, up to a
//!   thousand-fold.
//! - `Cache-Control: max-age`, RSS `<ttl>` and `Retry-After` are a floor
//!   under whichever of those came out.
//! - ±10% jitter, so a hundred sources imported from one OPML file do not
//!   all fire on the same minute forever.
//! - Last of all, clamped to `MIN_MINUTES..=MAX_MINUTES`, which is the
//!   day-long ceiling on a backoff.

use super::FetchState;
use jiff::Timestamp;

pub const DEFAULT_MINUTES: u32 = 60;
pub const MIN_MINUTES: u32 = 15;
pub const MAX_MINUTES: u32 = 24 * 60;
/// The longest a quiet source's interval is stretched to: a monthly blog is
/// asked every six hours, not every day, so a post that lands is found the
/// same morning.
pub const STRETCH_MAX_MINUTES: u32 = 6 * 60;
/// How many recent publication times [`FetchState::recent`] keeps.
pub const RECENT_KEPT: usize = 20;
/// A source failing for this many days in a row pauses itself.
pub const PAUSE_AFTER_DAYS: i64 = 30;

/// What the server said, as far as scheduling cares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hints {
    /// `Cache-Control: max-age`, in seconds.
    pub max_age_secs: Option<u32>,
    /// `Retry-After`, in seconds (an HTTP date already converted).
    pub retry_after_secs: Option<u32>,
    /// RSS `<ttl>`, or `sy:updatePeriod` x `sy:updateFrequency`, in minutes.
    pub ttl_minutes: Option<u32>,
}

/// What happened on this attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// New content arrived (or the source was read for the first time).
    Fetched,
    /// `304 Not Modified`, or a body identical to the last one.
    NotModified,
    /// It failed. `failures` on the state passed in does not yet count this
    /// one.
    Failed,
}

/// The interval to wait, in minutes, and the instant that makes.
///
/// `jitter` is in `-1.0..=1.0` and scales the ±10% spread; pass `0.0` for
/// none.
pub fn next_fetch(
    state: &FetchState,
    attempt: Attempt,
    hints: &Hints,
    now: Timestamp,
    jitter: f64,
) -> (u32, Timestamp) {
    let base = match attempt {
        Attempt::Fetched | Attempt::NotModified => {
            stretched(&state.recent).unwrap_or(DEFAULT_MINUTES)
        }
        // `failures` does not yet count this one, hence the `+ 1`: the first
        // failure already waits twice as long as a success would.
        Attempt::Failed => {
            let doublings = state.failures.saturating_add(1).min(MAX_DOUBLINGS);
            state.interval_minutes.max(DEFAULT_MINUTES).saturating_mul(1u32 << doublings)
        }
    };

    // What the server asked for is a floor, whichever way the base came out.
    let minutes = [
        base,
        hints.max_age_secs.map_or(0, |s| s / 60),
        hints.ttl_minutes.unwrap_or(0),
        hints.retry_after_secs.map_or(0, |s| s.div_ceil(60)),
    ]
    .into_iter()
    .max()
    .unwrap_or(base);

    let jitter = if jitter.is_nan() { 0.0 } else { jitter.clamp(-1.0, 1.0) };
    let spread = (f64::from(minutes) * (1.0 + 0.1 * jitter)).round();
    // `as` saturates, and the clamp below makes the exact value beyond
    // `u32::MAX` irrelevant.
    //
    // `Retry-After` is applied again after the jitter, and only it: a cache
    // lifetime or a `<ttl>` is a hint the spread may shave a few minutes off,
    // but a server that said "not before" means it, and asking ten percent
    // early is asking to be told again.
    let retry_after = hints.retry_after_secs.map_or(0, |s| s.div_ceil(60));
    let minutes = (spread as u32).max(retry_after).clamp(MIN_MINUTES, MAX_MINUTES);
    (minutes, add_minutes(now, minutes))
}

/// How many times a failing source's interval may double, so that a long
/// outage cannot overflow the arithmetic on its way to the day-long ceiling.
const MAX_DOUBLINGS: u32 = 10;

/// Half the median gap between recent publications, held between the default
/// and [`STRETCH_MAX_MINUTES`]. `None` while there are too few to say.
fn stretched(recent: &[Timestamp]) -> Option<u32> {
    // Sorted and de-duplicated here rather than trusted to `remember`: a gap
    // of zero between two copies of one time says nothing about how often the
    // source posts.
    let mut times: Vec<i64> = recent.iter().map(|t| t.as_second()).collect();
    times.sort_unstable_by(|a, b| b.cmp(a));
    times.dedup();
    if times.len() < 3 {
        return None;
    }
    let mut gaps: Vec<i64> = times.windows(2).map(|w| w[0] - w[1]).collect();
    gaps.sort_unstable();
    let mid = gaps.len() / 2;
    let median_secs = if gaps.len() % 2 == 1 { gaps[mid] } else { (gaps[mid - 1] + gaps[mid]) / 2 };
    let half = median_secs / 60 / 2;
    let clamped = half.clamp(i64::from(DEFAULT_MINUTES), i64::from(STRETCH_MAX_MINUTES));
    u32::try_from(clamped).ok()
}

/// Fold newly seen publication times into `recent`: newest first, no
/// duplicates, at most [`RECENT_KEPT`].
pub fn remember(recent: &mut Vec<Timestamp>, seen: impl IntoIterator<Item = Timestamp>) {
    recent.extend(seen);
    recent.sort_by(|a, b| b.cmp(a));
    recent.dedup();
    recent.truncate(RECENT_KEPT);
}

/// Has this source been failing long enough to pause itself?
pub fn should_pause(state: &FetchState, now: Timestamp) -> bool {
    let Some(since) = state.failing_since else { return false };
    now.as_second() - since.as_second() >= PAUSE_AFTER_DAYS * 24 * 3600
}

fn add_minutes(t: Timestamp, minutes: u32) -> Timestamp {
    t.checked_add(jiff::SignedDuration::from_mins(i64::from(minutes))).unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::NewsSourceId;
    use jiff::SignedDuration;

    fn now() -> Timestamp {
        "2026-10-11T12:00:00Z".parse().unwrap()
    }

    fn mins(n: i64) -> SignedDuration {
        SignedDuration::from_mins(n)
    }

    /// Publication times, newest first, an hour before `now` and then each
    /// `gaps[i]` minutes before the last.
    fn posted_with_gaps(gaps: &[i64]) -> Vec<Timestamp> {
        let mut at = now() - mins(60);
        let mut times = vec![at];
        for gap in gaps {
            at -= mins(*gap);
            times.push(at);
        }
        times
    }

    /// `n` publication times, `gap` minutes apart.
    fn posted_every(gap: i64, n: usize) -> Vec<Timestamp> {
        posted_with_gaps(&vec![gap; n - 1])
    }

    fn state(failures: u32, interval_minutes: u32, recent: Vec<Timestamp>) -> FetchState {
        let mut s = FetchState::new(NewsSourceId::new());
        s.failures = failures;
        s.interval_minutes = interval_minutes;
        s.recent = recent;
        s
    }

    fn hints(max_age_secs: Option<u32>, retry_after_secs: Option<u32>, ttl: Option<u32>) -> Hints {
        Hints { max_age_secs, retry_after_secs, ttl_minutes: ttl }
    }

    struct Case {
        name: &'static str,
        state: FetchState,
        attempt: Attempt,
        hints: Hints,
        jitter: f64,
        minutes: u32,
    }

    fn case(
        name: &'static str,
        state: FetchState,
        attempt: Attempt,
        hints: Hints,
        jitter: f64,
        minutes: u32,
    ) -> Case {
        Case { name, state, attempt, hints, jitter, minutes }
    }

    const MONTH: i64 = 30 * 24 * 60;
    const WEEK: i64 = 7 * 24 * 60;
    const NONE: Hints = Hints { max_age_secs: None, retry_after_secs: None, ttl_minutes: None };

    #[test]
    fn the_next_fetch_follows_the_table() {
        use Attempt::{Failed, Fetched, NotModified};
        let cases = vec![
            // ── What a success waits ─────────────────────────────────────
            case("default", state(0, 0, vec![]), Fetched, NONE, 0.0, 60),
            case("default after a 304", state(0, 0, vec![]), NotModified, NONE, 0.0, 60),
            case(
                "two posts are too few to stretch to",
                state(0, 0, posted_every(MONTH, 2)),
                Fetched,
                NONE,
                0.0,
                60,
            ),
            case("a monthly blog", state(0, 0, posted_every(MONTH, 4)), Fetched, NONE, 0.0, 360),
            case("a weekly one", state(0, 0, posted_every(WEEK, 4)), NotModified, NONE, 0.0, 360),
            case(
                "a post every four hours",
                state(0, 0, posted_every(240, 5)),
                Fetched,
                NONE,
                0.0,
                120,
            ),
            case("a busy feed", state(0, 0, posted_every(10, 20)), Fetched, NONE, 0.0, 60),
            case(
                "the median, not the mean: one long silence among short gaps",
                state(0, 0, posted_with_gaps(&[240, 240, 100_000, 240])),
                Fetched,
                NONE,
                0.0,
                120,
            ),
            case(
                "copies of one time are not a gap of zero",
                state(0, 0, [posted_every(WEEK, 3), posted_every(WEEK, 3)].concat()),
                Fetched,
                NONE,
                0.0,
                360,
            ),
            // ── What the server asks for is a floor ──────────────────────
            case(
                "max-age above the base",
                state(0, 0, vec![]),
                Fetched,
                hints(Some(7200), None, None),
                0.0,
                120,
            ),
            case(
                "max-age below the base",
                state(0, 0, vec![]),
                Fetched,
                hints(Some(600), None, None),
                0.0,
                60,
            ),
            case(
                "ttl above the base",
                state(0, 0, vec![]),
                Fetched,
                hints(None, None, Some(90)),
                0.0,
                90,
            ),
            case(
                "ttl below the base",
                state(0, 0, vec![]),
                Fetched,
                hints(None, None, Some(30)),
                0.0,
                60,
            ),
            case(
                "ttl above a stretched interval",
                state(0, 0, posted_every(MONTH, 4)),
                Fetched,
                hints(None, None, Some(720)),
                0.0,
                720,
            ),
            case(
                "the largest of the hints wins",
                state(0, 0, vec![]),
                Fetched,
                hints(Some(9000), None, Some(200)),
                0.0,
                200,
            ),
            case(
                "a hint cannot pass the ceiling",
                state(0, 0, vec![]),
                Fetched,
                hints(None, None, Some(5000)),
                0.0,
                1440,
            ),
            // ── Retry-After ──────────────────────────────────────────────
            case(
                "retry-after is rounded up to a whole minute",
                state(0, 0, vec![]),
                Failed,
                hints(None, Some(10_801), None),
                0.0,
                181,
            ),
            case(
                "retry-after below the backoff changes nothing",
                state(0, 0, vec![]),
                Failed,
                hints(None, Some(600), None),
                0.0,
                120,
            ),
            case(
                "retry-after is capped at a day",
                state(0, 0, vec![]),
                Failed,
                hints(None, Some(100_000), None),
                0.0,
                1440,
            ),
            // ── What a failure waits ─────────────────────────────────────
            case("the first failure", state(0, 0, vec![]), Failed, NONE, 0.0, 120),
            case(
                "the first failure after a stretched interval",
                state(0, 360, vec![]),
                Failed,
                NONE,
                0.0,
                720,
            ),
            case("the second failure", state(1, 60, vec![]), Failed, NONE, 0.0, 240),
            case(
                "the tenth failure is capped at a day",
                state(9, 60, vec![]),
                Failed,
                NONE,
                0.0,
                1440,
            ),
            case(
                "and so is the four-billionth",
                state(4_000_000_000, 60, vec![]),
                Failed,
                NONE,
                0.0,
                1440,
            ),
            case(
                "and one that overflows the count",
                state(u32::MAX, 1440, vec![]),
                Failed,
                NONE,
                0.0,
                1440,
            ),
            case(
                "a failure is not stretched by how often the source posts",
                state(0, 0, posted_every(MONTH, 4)),
                Failed,
                NONE,
                0.0,
                120,
            ),
            // ── Jitter ───────────────────────────────────────────────────
            case(
                "jitter of one is ten per cent longer",
                state(0, 0, vec![]),
                Fetched,
                NONE,
                1.0,
                66,
            ),
            case(
                "jitter of minus one is ten per cent shorter",
                state(0, 0, vec![]),
                Fetched,
                NONE,
                -1.0,
                54,
            ),
            case("half a jitter is five per cent", state(0, 0, vec![]), Fetched, NONE, 0.5, 63),
            case("a jitter beyond one is held to one", state(0, 0, vec![]), Fetched, NONE, 5.0, 66),
            case(
                "a jitter that is not a number is none",
                state(0, 0, vec![]),
                Fetched,
                NONE,
                f64::NAN,
                60,
            ),
            case("jitter cannot pass the ceiling", state(9, 60, vec![]), Failed, NONE, -1.0, 1440),
        ];

        for c in cases {
            let (minutes, at) = next_fetch(&c.state, c.attempt, &c.hints, now(), c.jitter);
            assert_eq!(minutes, c.minutes, "{}", c.name);
            assert_eq!(at, now() + mins(i64::from(c.minutes)), "{}: the instant", c.name);
        }
    }

    #[test]
    fn whatever_happens_the_wait_stays_between_the_floor_and_the_ceiling() {
        let greedy = hints(Some(u32::MAX), Some(u32::MAX), Some(u32::MAX));
        for failures in [0, 1, 5, 9, 50] {
            for attempt in [Attempt::Fetched, Attempt::NotModified, Attempt::Failed] {
                for hints in [NONE, greedy] {
                    for jitter in [-1.0, -0.3, 0.0, 0.7, 1.0] {
                        let s = state(failures, 700, posted_every(MONTH, 5));
                        let (minutes, _) = next_fetch(&s, attempt, &hints, now(), jitter);
                        assert!(
                            (MIN_MINUTES..=MAX_MINUTES).contains(&minutes),
                            "{attempt:?} after {failures} failures, jitter {jitter}: {minutes}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_source_pauses_after_thirty_days_of_failing_and_not_before() {
        let mut s = state(40, 1440, vec![]);
        assert!(!should_pause(&s, now()), "healthy");

        s.failing_since = Some(now() - SignedDuration::from_hours(24 * 29));
        assert!(!should_pause(&s, now()), "29 days");

        s.failing_since = Some(now() - SignedDuration::from_hours(24 * 30));
        assert!(should_pause(&s, now()), "30 days");

        s.failing_since = Some(now() - SignedDuration::from_hours(24 * 31));
        assert!(should_pause(&s, now()), "31 days");
    }

    #[test]
    fn remembering_keeps_the_newest_first_without_copies_and_no_more_than_twenty() {
        let mut recent = vec![now() - mins(30)];
        remember(&mut recent, [now() - mins(90), now() - mins(30), now() - mins(10)]);
        assert_eq!(recent, vec![now() - mins(10), now() - mins(30), now() - mins(90)]);

        remember(&mut recent, (1..=40).map(|i| now() - mins(1000 + i)));
        assert_eq!(recent.len(), RECENT_KEPT);
        assert_eq!(recent[0], now() - mins(10), "the newest survive the cut");
    }
}

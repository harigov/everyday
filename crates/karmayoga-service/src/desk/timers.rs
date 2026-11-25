//! The timers the assistant set: "a timer for ten minutes", "wake me at
//! seven".
//!
//! In memory, held by the [`Service`](crate::service::Service), and gone
//! when the app quits -- a timer is for the pasta; anything that must
//! survive a restart is a task's reminder, which `set_timer` says when it
//! refuses one longer than a day. Each runs on a task of its own that rings
//! it through the [`Desk`] and forgets it.
//!
//! # Wall-clock time, checked as it goes
//!
//! A timer sleeps in steps of at most [`STEP`] and looks at the wall clock
//! after each, rather than sleeping once for the whole length. Tokio's clock
//! is monotonic, and on Linux the monotonic clock stops while the machine
//! sleeps: an alarm for seven set the night before on a laptop that was
//! closed overnight would otherwise ring the length of the night late. Read
//! the wall clock, and it rings within a step of the lid opening.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;

use super::{Desk, Timer};
use crate::clock::Clock;

/// The most timers running at once. Enough for a kitchen; a model setting
/// timers in a loop is stopped well short of anything that matters.
pub const MOST_TIMERS: usize = 20;

/// The longest a timer sleeps before looking at the clock again.
const STEP: Duration = Duration::from_secs(15);

/// The longest a label may be: a word or two, "pasta", "call Mum back".
pub const MAX_LABEL_CHARS: usize = 80;

/// The timers running now. Cheap to clone; every clone is the same set.
#[derive(Clone, Default)]
pub struct Timers {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// The last id handed out; ids count up from 1 for the life of the
    /// process, so "timer 2" never comes to mean a different one.
    last: u32,
    running: Vec<Running>,
}

struct Running {
    timer: Timer,
    task: tokio::task::AbortHandle,
}

impl Timers {
    /// Start a timer that rings at `ends` through `desk`. The error is in
    /// words fit to tell the person.
    pub fn set(
        &self,
        desk: Arc<dyn Desk>,
        clock: Arc<dyn Clock>,
        label: Option<String>,
        ends: Timestamp,
        zone: &str,
    ) -> Result<Timer, String> {
        let label = label
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .map(|l| l.chars().take(MAX_LABEL_CHARS).collect::<String>());
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.running.len() >= MOST_TIMERS {
            return Err(format!(
                "There are already {MOST_TIMERS} timers running; cancel one first."
            ));
        }
        inner.last += 1;
        let timer = Timer { id: inner.last, label, set: clock.now(), ends, zone: zone.to_string() };
        let task = tokio::spawn(run(self.clone(), desk, clock, timer.clone()));
        inner.running.push(Running { timer: timer.clone(), task: task.abort_handle() });
        Ok(timer)
    }

    /// The timers running, soonest first.
    pub fn list(&self) -> Vec<Timer> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut timers: Vec<Timer> = inner.running.iter().map(|r| r.timer.clone()).collect();
        timers.sort_by_key(|t| (t.ends, t.id));
        timers
    }

    /// Stop timer `id` before it rings. `None` when there is no such timer
    /// -- it has rung already, or never was.
    pub fn cancel(&self, id: u32) -> Option<Timer> {
        let running = self.take(id)?;
        running.task.abort();
        Some(running.timer)
    }

    /// Stop every timer. The ones that were running, soonest first.
    pub fn cancel_all(&self) -> Vec<Timer> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut stopped: Vec<Running> = std::mem::take(&mut inner.running);
        drop(inner);
        stopped.sort_by_key(|r| (r.timer.ends, r.timer.id));
        stopped
            .into_iter()
            .map(|r| {
                r.task.abort();
                r.timer
            })
            .collect()
    }

    /// Take timer `id` off the list, so it is rung once and cancelled never
    /// after it has rung.
    fn take(&self, id: u32) -> Option<Running> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let at = inner.running.iter().position(|r| r.timer.id == id)?;
        Some(inner.running.remove(at))
    }
}

/// One timer's task: sleep until it ends, by the wall clock, then ring it --
/// unless it was cancelled in between, which aborts this task, or taken off
/// the list just as it woke.
async fn run(timers: Timers, desk: Arc<dyn Desk>, clock: Arc<dyn Clock>, timer: Timer) {
    loop {
        let left = clock.now().duration_until(timer.ends);
        if left.is_zero() || left.is_negative() {
            break;
        }
        let left = Duration::try_from(left).unwrap_or(STEP);
        tokio::time::sleep(left.min(STEP)).await;
    }
    if timers.take(timer.id).is_some() {
        desk.ring(timer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::desk::testing::FakeDesk;
    use jiff::SignedDuration;

    /// What the desk was asked to ring, by id.
    fn rung(desk: &FakeDesk) -> Vec<String> {
        desk.asked()
    }

    fn in_ms(ms: i64) -> Timestamp {
        Timestamp::now().checked_add(SignedDuration::from_millis(ms)).unwrap()
    }

    #[tokio::test]
    async fn a_timer_rings_once_when_it_ends_and_is_forgotten() {
        let bell = Arc::new(FakeDesk::default());
        let timers = Timers::default();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let timer =
            timers.set(bell.clone(), clock, Some("  pasta \n ".into()), in_ms(50), "UTC").unwrap();
        assert_eq!(timer.id, 1);
        assert_eq!(timer.label.as_deref(), Some("pasta"));
        assert_eq!(timers.list(), vec![timer.clone()]);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(rung(&bell), vec![format!("ring {}", timer.id)]);
        assert!(timers.list().is_empty());
        assert!(timers.cancel(timer.id).is_none(), "a rung timer cannot be cancelled");
    }

    #[tokio::test]
    async fn a_cancelled_timer_never_rings_and_ids_are_not_reused() {
        let bell = Arc::new(FakeDesk::default());
        let timers = Timers::default();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let first = timers.set(bell.clone(), clock.clone(), None, in_ms(100), "UTC").unwrap();
        let second = timers.set(bell.clone(), clock.clone(), None, in_ms(60_000), "UTC").unwrap();
        assert_eq!(timers.list().iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(timers.cancel(first.id), Some(first));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(rung(&bell).is_empty());
        let third = timers.set(bell.clone(), clock, None, in_ms(60_000), "UTC").unwrap();
        assert_eq!(third.id, 3);
        assert_eq!(timers.cancel_all(), vec![second, third]);
        assert!(timers.list().is_empty());
    }

    #[tokio::test]
    async fn there_is_a_most_timers_at_once() {
        let bell = Arc::new(FakeDesk::default());
        let timers = Timers::default();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        for _ in 0..MOST_TIMERS {
            timers.set(bell.clone(), clock.clone(), None, in_ms(60_000), "UTC").unwrap();
        }
        assert!(timers.set(bell.clone(), clock, None, in_ms(60_000), "UTC").is_err());
        timers.cancel_all();
    }
}

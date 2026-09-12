//! Standing work: what the assistant does without being asked.
//!
//! A [`Routine`] is a trigger, an instruction in the person's own words, and
//! a switch. "Every weekday at seven, look at what is due and what is on the
//! calendar, and leave me a note." The scheduler in `everyday-service` reads
//! them, decides which are due, and runs them one at a time through the same
//! turn the chat rail uses.
//!
//! # Why the arithmetic is here
//!
//! Deciding whether a routine is due is the part that goes wrong, and it goes
//! wrong in ways that only show up at a boundary: a weekday schedule across a
//! weekend, an hour that happens twice when the clocks go back, a laptop that
//! was shut for a week. So it is a pure function of a trigger, a clock and a
//! zone, with the clock passed in -- and there is a table of cases at the
//! foot of this file rather than a scheduler somebody has to run in October
//! to find out about.
//!
//! # The rule about missing one
//!
//! A morning brief that was missed by six hours is not a morning brief. A
//! weekly review missed by a day is still a weekly review. So a routine
//! carries a *grace*, and a slot older than that is [`Due::Missed`] -- which
//! the scheduler records as a skipped run with a reason, rather than either
//! running six hours late or saying nothing at all. A laptop shut for a week
//! shows one honest row per morning instead of seven briefs at once.

use crate::id::{RoleId, RoutineId, RoutineRunId};
use crate::{ConversationId, Error, Result};
use jiff::civil::{Time, Weekday as CivilWeekday};
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

/// Longest a routine's instructions may be.
///
/// The same order as the assistant's own standing instructions, and finite
/// for the same reason: this string is sent on every run of every day.
pub const MAX_INSTRUCTIONS_BYTES: usize = 8_000;

/// Longest a routine's name may be.
pub const MAX_NAME_BYTES: usize = 200;

/// How long after its moment a routine will still run, when nothing else is
/// said. An hour: long enough to survive a laptop opened after breakfast,
/// short enough that a morning brief is still a morning brief.
pub const DEFAULT_GRACE_MINUTES: u32 = 60;

/// A day of the week, in the spelling the wire uses.
///
/// Ours rather than [`jiff::civil::Weekday`]'s so that the stored spelling is
/// this application's decision and cannot change under us with a dependency
/// bump. The conversion is the only place the two meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    pub const ALL: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    /// Monday to Friday, which is what "weekdays" means to a person and is
    /// the commonest schedule anybody will ask for.
    pub const WEEKDAYS: [Weekday; 5] =
        [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri];

    pub fn as_str(self) -> &'static str {
        match self {
            Weekday::Mon => "mon",
            Weekday::Tue => "tue",
            Weekday::Wed => "wed",
            Weekday::Thu => "thu",
            Weekday::Fri => "fri",
            Weekday::Sat => "sat",
            Weekday::Sun => "sun",
        }
    }

    /// Parse a day. Accepts the short form, the full name, and any casing,
    /// because this is also what a model will be typing into `create_routine`.
    pub fn parse(s: &str) -> Option<Weekday> {
        let s = s.trim().to_lowercase();
        Weekday::ALL.iter().copied().find(|d| {
            let short = d.as_str();
            s == short || s.starts_with(short) && d.full().starts_with(&s)
        })
    }

    pub fn full(self) -> &'static str {
        match self {
            Weekday::Mon => "monday",
            Weekday::Tue => "tuesday",
            Weekday::Wed => "wednesday",
            Weekday::Thu => "thursday",
            Weekday::Fri => "friday",
            Weekday::Sat => "saturday",
            Weekday::Sun => "sunday",
        }
    }

    fn from_civil(day: CivilWeekday) -> Weekday {
        match day {
            CivilWeekday::Monday => Weekday::Mon,
            CivilWeekday::Tuesday => Weekday::Tue,
            CivilWeekday::Wednesday => Weekday::Wed,
            CivilWeekday::Thursday => Weekday::Thu,
            CivilWeekday::Friday => Weekday::Fri,
            CivilWeekday::Saturday => Weekday::Sat,
            CivilWeekday::Sunday => Weekday::Sun,
        }
    }
}

/// What sets a routine going.
///
/// An enum rather than a bag of optional fields, for the reason
/// [`crate::Purpose`] is one: a clock time with weekdays and a lead time
/// before a meeting have nothing in common but the word "when", and a routine
/// has exactly one of them.
// Both lines matter and they do different things: `rename_all` names the
// variants, `rename_all_fields` names the fields inside them. Without the
// second, `lead_minutes` went out in snake_case while every other record on
// the wire is camelCase -- so a routine that runs before a meeting could not
// be saved from the interface at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Trigger {
    /// A time of day, on the given days. An empty list means every day.
    Schedule {
        at: Time,
        #[serde(default)]
        days: Vec<Weekday>,
    },
    /// Before an event on the calendar starts.
    ///
    /// Evaluated as a *query* on each tick rather than as a clock slot: the
    /// events it is about arrive from somebody else's server, so there is no
    /// moment to compute in advance. See [`Trigger::is_clock`].
    BeforeEvent {
        lead_minutes: u32,
        /// Only events on a calendar filed under this role. `None` is any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_id: Option<RoleId>,
    },
    /// Before a task falls due. A query, like `BeforeEvent`.
    TaskDue { lead_days: u32 },
    /// Never on its own. Run now, and nothing else.
    Manual,
}

impl Trigger {
    /// Whether this trigger is a moment on the clock.
    ///
    /// The two query triggers are answered by looking at what is in the vault
    /// on each tick, so [`Routine::is_due`] has nothing to say about them.
    pub fn is_clock(&self) -> bool {
        matches!(self, Trigger::Schedule { .. })
    }

    /// The trigger in words, for a list that has to say when something runs.
    ///
    /// Here rather than in the interface because the assistant has to say it
    /// too, and two spellings of "weekdays at 07:00" would eventually
    /// disagree.
    pub fn describe(&self) -> String {
        match self {
            Trigger::Schedule { at, days } => {
                let when = format!("{:02}:{:02}", at.hour(), at.minute());
                if days.is_empty() {
                    return format!("Every day at {when}");
                }
                if days.len() == 7 {
                    return format!("Every day at {when}");
                }
                if days.len() == 5 && Weekday::WEEKDAYS.iter().all(|d| days.contains(d)) {
                    return format!("Weekdays at {when}");
                }
                if days.len() == 2 && days.contains(&Weekday::Sat) && days.contains(&Weekday::Sun) {
                    return format!("Weekends at {when}");
                }
                let names: Vec<&str> =
                    Weekday::ALL.iter().filter(|d| days.contains(d)).map(|d| d.as_str()).collect();
                format!("{} at {when}", capitalise(&names.join(", ")))
            }
            Trigger::BeforeEvent { lead_minutes, .. } => {
                format!("{lead_minutes} minutes before a meeting")
            }
            Trigger::TaskDue { lead_days } => match lead_days {
                0 => "When a task falls due".into(),
                1 => "The day before a task is due".into(),
                n => format!("{n} days before a task is due"),
            },
            Trigger::Manual => "Only when you ask".into(),
        }
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Whether a routine wants running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Its moment has come and is still within its grace. `slot` is the
    /// moment, which the run records so two ticks cannot both claim it.
    Now { slot: Timestamp },
    /// Its moment came and went while nothing was running. Recorded as a
    /// skipped run rather than run late or passed over in silence.
    Missed { slot: Timestamp },
    /// Nothing to do.
    Later,
}

/// Standing work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    pub id: RoutineId,
    pub name: String,
    /// What to do, in the person's own words. This is the prompt.
    pub instructions: String,
    pub trigger: Trigger,
    /// Minutes past its moment that it will still run. See the module note.
    #[serde(default = "default_grace")]
    pub grace_minutes: u32,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn default_grace() -> u32 {
    DEFAULT_GRACE_MINUTES
}

impl Routine {
    pub fn new(name: impl Into<String>, instructions: impl Into<String>, trigger: Trigger) -> Self {
        let now = Timestamp::now();
        Self {
            id: RoutineId::new(),
            name: name.into(),
            instructions: instructions.into(),
            trigger,
            grace_minutes: DEFAULT_GRACE_MINUTES,
            enabled: true,
            last_run_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Invalid("a routine needs a name".into()));
        }
        if self.name.len() > MAX_NAME_BYTES {
            return Err(Error::Invalid(format!(
                "a routine's name must be under {MAX_NAME_BYTES} bytes"
            )));
        }
        if self.instructions.trim().is_empty() {
            return Err(Error::Invalid(
                "a routine needs instructions: what should it do when it runs?".into(),
            ));
        }
        if self.instructions.len() > MAX_INSTRUCTIONS_BYTES {
            return Err(Error::Invalid(format!(
                "instructions are {} bytes; the limit is {MAX_INSTRUCTIONS_BYTES}",
                self.instructions.len()
            )));
        }
        Ok(())
    }

    /// The moment this routine last wanted running at or before `now`.
    ///
    /// `None` for a trigger that is not on the clock, and for a schedule
    /// whose days do not include the last seven -- which cannot happen, since
    /// an empty list means every day, but is expressed rather than assumed.
    pub fn slot_at_or_before(&self, now: &Zoned) -> Option<Timestamp> {
        let Trigger::Schedule { at, days } = &self.trigger else { return None };
        // Walk back a week a day at a time. Cheap, and it is the only way to
        // be right across a zone whose offset changed in the middle: each
        // candidate is built in the local calendar and converted, so a slot
        // is the wall-clock time it says it is.
        for back in 0..=7 {
            let day = now.date().checked_sub(jiff::Span::new().days(back)).ok()?;
            if !days.is_empty() && !days.contains(&Weekday::from_civil(day.weekday())) {
                continue;
            }
            // An hour that does not exist -- the spring-forward gap -- is
            // taken as the first instant that does, which is what a person
            // means by "at half past two" on that one morning.
            let Ok(candidate) = day.to_datetime(*at).to_zoned(now.time_zone().clone()) else {
                continue;
            };
            if candidate.timestamp() <= now.timestamp() {
                return Some(candidate.timestamp());
            }
        }
        None
    }

    /// Whether this routine wants running now.
    ///
    /// `since` is the floor: the last run, else the moment the routine was
    /// made. Using `created_at` as the floor is what stops a routine set up
    /// at nine in the morning reporting that it missed its seven o'clock --
    /// a slot that existed before the routine did is not one it missed.
    pub fn is_due(&self, now: &Zoned) -> Due {
        if !self.enabled || !self.trigger.is_clock() {
            return Due::Later;
        }
        let Some(slot) = self.slot_at_or_before(now) else { return Due::Later };
        let since = self.last_run_at.unwrap_or(self.created_at);
        if slot <= since {
            return Due::Later;
        }
        let late = now.timestamp().as_second() - slot.as_second();
        if late <= i64::from(self.grace_minutes) * 60 {
            Due::Now { slot }
        } else {
            Due::Missed { slot }
        }
    }

    /// The next moment this routine will want running, strictly after `now`.
    ///
    /// For a list that says when something runs next. `None` for a trigger
    /// that is not on the clock, or a routine that is switched off.
    pub fn next_due(&self, now: &Zoned) -> Option<Timestamp> {
        if !self.enabled {
            return None;
        }
        let Trigger::Schedule { at, days } = &self.trigger else { return None };
        for ahead in 0..=7 {
            let day = now.date().checked_add(jiff::Span::new().days(ahead)).ok()?;
            if !days.is_empty() && !days.contains(&Weekday::from_civil(day.weekday())) {
                continue;
            }
            let Ok(candidate) = day.to_datetime(*at).to_zoned(now.time_zone().clone()) else {
                continue;
            };
            if candidate.timestamp() > now.timestamp() {
                return Some(candidate.timestamp());
            }
        }
        None
    }
}

/// How a run ended.
///
/// A closed set, like [`crate::GoalStatus`], so a run cannot be in a state
/// the interface has no word for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// Asked for, and not started yet.
    ///
    /// Its own state rather than a corner of `Running`, because the two look
    /// identical in the vault and want opposite things from the scheduler: a
    /// queued run is work to pick up, and a running one whose process is gone
    /// is a row to close. Guessing between them from a timestamp would be a
    /// guess that eventually eats somebody's "run now".
    #[default]
    Queued,
    /// Started and has not finished. A row in this state that no live process
    /// claims is one a dead process left; see the scheduler's sweep.
    Running,
    Done,
    /// The model refused, the endpoint was unreachable, the turn timed out.
    Failed,
    /// Never started, and why is in `reason`: its moment was missed, the
    /// assistant is switched off, the vault was locked.
    Skipped,
}

impl Outcome {
    pub const ALL: [Outcome; 5] =
        [Outcome::Queued, Outcome::Running, Outcome::Done, Outcome::Failed, Outcome::Skipped];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Queued => "queued",
            Outcome::Running => "running",
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
        }
    }

    /// Whether this run is over, one way or another.
    pub fn is_finished(self) -> bool {
        !matches!(self, Outcome::Queued | Outcome::Running)
    }
}

/// One run of a routine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRun {
    pub id: RoutineRunId,
    pub routine_id: RoutineId,
    /// What this routine was called when it ran, so a log row still reads
    /// after the routine is renamed or deleted.
    #[serde(default)]
    pub routine_name: String,
    /// The scheduled moment this run is for, so two ticks cannot both claim
    /// one. Absent for a run somebody asked for by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<Timestamp>,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    pub outcome: Outcome,
    /// Why it failed or was skipped. Empty when it simply worked.
    #[serde(default)]
    pub reason: String,
    /// What it was about: the meeting, the task. Only the query triggers set
    /// one, and it is also how a second run for the same meeting is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The transcript. Absent for a run that never reached the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    /// The model's last message: what it has to say for itself.
    #[serde(default)]
    pub summary: String,
    /// Whether anybody has looked at it. What the count on the app bar is.
    #[serde(default)]
    pub seen: bool,
    #[serde(default)]
    pub steps: u32,
}

impl RoutineRun {
    pub fn new(routine: &Routine, slot: Option<Timestamp>) -> Self {
        Self {
            id: RoutineRunId::new(),
            routine_id: routine.id,
            routine_name: routine.name.clone(),
            slot,
            started_at: Timestamp::now(),
            finished_at: None,
            outcome: Outcome::Queued,
            reason: String::new(),
            subject: None,
            conversation_id: None,
            summary: String::new(),
            seen: false,
            steps: 0,
        }
    }

    /// A run that never started, and why.
    ///
    /// Recorded rather than logged, because the person's question the next
    /// morning is "why did I not get my brief", and a log line on a machine
    /// under a desk is not an answer.
    pub fn skipped(routine: &Routine, slot: Option<Timestamp>, reason: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            outcome: Outcome::Skipped,
            reason: reason.into(),
            finished_at: Some(now),
            // Nothing to look at, so nothing to mark as looked at. A skipped
            // run must not put a number on the app bar.
            seen: true,
            ..Self::new(routine, slot)
        }
    }

    /// Mark this run as under way, now.
    ///
    /// `started_at` is re-stamped because a queued run may have waited: the
    /// log should say when the work happened rather than when it was asked
    /// for.
    pub fn begin(&mut self) {
        self.outcome = Outcome::Running;
        self.started_at = Timestamp::now();
    }

    pub fn finish(&mut self, outcome: Outcome, summary: impl Into<String>) {
        self.outcome = outcome;
        self.summary = summary.into();
        self.finished_at = Some(Timestamp::now());
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.reason = reason.into();
        self.finish(Outcome::Failed, String::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    /// A routine that has never run, made a week before the tests look at it.
    fn morning(days: &[Weekday]) -> Routine {
        let mut r = Routine::new(
            "Morning brief",
            "Say what is due today.",
            Trigger::Schedule { at: time(7, 0, 0, 0), days: days.to_vec() },
        );
        r.created_at = date(2026, 9, 1).at(0, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
        r
    }

    fn at(y: i16, m: i8, d: i8, h: i8, min: i8, zone: &str) -> Zoned {
        date(y, m, d).at(h, min, 0, 0).in_tz(zone).expect("a real local time")
    }

    #[test]
    fn a_routine_is_due_at_its_hour_and_not_before() {
        let mut r = morning(&[]);
        // The steady state: it ran yesterday, as it does every day.
        r.last_run_at = Some(at(2026, 9, 7, 7, 0, "UTC").timestamp());
        assert_eq!(r.is_due(&at(2026, 9, 8, 6, 59, "UTC")), Due::Later, "not yet");
        assert!(matches!(r.is_due(&at(2026, 9, 8, 7, 0, "UTC")), Due::Now { .. }), "on the hour");
        assert!(matches!(r.is_due(&at(2026, 9, 8, 7, 59, "UTC")), Due::Now { .. }), "within grace");
    }

    #[test]
    fn a_slot_missed_by_more_than_its_grace_is_missed_rather_than_run_late() {
        let r = morning(&[]);
        // A morning brief at half past two in the afternoon is not a morning
        // brief. The scheduler writes a skipped row saying so.
        assert!(matches!(r.is_due(&at(2026, 9, 8, 14, 30, "UTC")), Due::Missed { .. }));
    }

    #[test]
    fn a_routine_that_has_already_run_for_its_slot_is_not_due_again() {
        let mut r = morning(&[]);
        let Due::Now { slot } = r.is_due(&at(2026, 9, 8, 7, 5, "UTC")) else {
            panic!("should be due");
        };
        r.last_run_at = Some(slot);
        assert_eq!(r.is_due(&at(2026, 9, 8, 7, 30, "UTC")), Due::Later, "one run per slot");
        // And the next morning it is due again.
        assert!(matches!(r.is_due(&at(2026, 9, 9, 7, 5, "UTC")), Due::Now { .. }));
    }

    #[test]
    fn a_weekday_routine_sleeps_through_the_weekend() {
        // 12 September 2026 is a Saturday.
        let mut r = morning(&Weekday::WEEKDAYS);
        r.last_run_at = Some(at(2026, 9, 11, 7, 0, "UTC").timestamp());
        assert_eq!(
            r.is_due(&at(2026, 9, 12, 7, 5, "UTC")),
            Due::Later,
            "Saturday is not a weekday"
        );
        assert_eq!(r.is_due(&at(2026, 9, 13, 7, 5, "UTC")), Due::Later, "nor is Sunday");
        assert!(matches!(r.is_due(&at(2026, 9, 14, 7, 5, "UTC")), Due::Now { .. }), "Monday is");
    }

    #[test]
    fn a_slot_that_existed_before_the_routine_did_is_not_one_it_missed() {
        // Set up at nine in the morning, for seven in the morning. This
        // morning's seven o'clock is not a slot it failed to run.
        let mut r = morning(&[]);
        r.created_at = at(2026, 9, 8, 9, 0, "UTC").timestamp();
        assert_eq!(r.is_due(&at(2026, 9, 8, 9, 1, "UTC")), Due::Later);
        assert!(
            matches!(r.is_due(&at(2026, 9, 9, 7, 5, "UTC")), Due::Now { .. }),
            "and tomorrow's is"
        );
    }

    #[test]
    fn a_switched_off_routine_is_never_due() {
        let mut r = morning(&[]);
        r.enabled = false;
        assert_eq!(r.is_due(&at(2026, 9, 8, 7, 5, "UTC")), Due::Later);
        assert_eq!(r.next_due(&at(2026, 9, 8, 7, 5, "UTC")), None);
    }

    #[test]
    fn a_manual_routine_is_never_due_on_the_clock() {
        let r = Routine::new("On demand", "Do the thing.", Trigger::Manual);
        assert_eq!(r.is_due(&at(2026, 9, 8, 7, 5, "UTC")), Due::Later);
        assert!(!r.trigger.is_clock());
    }

    #[test]
    fn the_hour_is_the_persons_hour_and_it_survives_the_clocks_going_back() {
        // 1 November 2026, America/Los_Angeles: 01:00 happens twice. A
        // routine set for 01:30 must fire once, at a real instant, and be
        // reckoned in the local calendar rather than by adding 86 400
        // seconds to yesterday.
        let mut r = morning(&[]);
        r.trigger = Trigger::Schedule { at: time(1, 30, 0, 0), days: vec![] };
        r.created_at = at(2026, 10, 30, 0, 0, "America/Los_Angeles").timestamp();

        let after = at(2026, 11, 1, 4, 0, "America/Los_Angeles");
        let (Due::Now { slot } | Due::Missed { slot }) = r.is_due(&after) else {
            panic!("a slot exists on the long day")
        };
        let local = slot.to_zoned(after.time_zone().clone());
        assert_eq!(local.hour(), 1, "the wall clock said half past one: got {local}");
        assert_eq!(local.minute(), 30);
    }

    #[test]
    fn the_hour_survives_the_clocks_going_forward_over_the_gap() {
        // 8 March 2026, America/Los_Angeles: 02:00 to 03:00 does not exist. A
        // routine set for 02:30 still has to have a moment that day, and it
        // is the first instant that does exist.
        let mut r = morning(&[]);
        r.trigger = Trigger::Schedule { at: time(2, 30, 0, 0), days: vec![] };
        r.created_at = at(2026, 3, 7, 0, 0, "America/Los_Angeles").timestamp();

        let after = at(2026, 3, 8, 9, 0, "America/Los_Angeles");
        let slot = r.slot_at_or_before(&after).expect("the gap must not swallow the day");
        let local = slot.to_zoned(after.time_zone().clone());
        assert_eq!(local.date(), date(2026, 3, 8), "it is still that morning: got {local}");
    }

    #[test]
    fn next_due_is_the_following_slot_and_never_this_one() {
        let r = morning(&Weekday::WEEKDAYS);
        // Friday 11 September, after seven: the next is Monday.
        let next = r.next_due(&at(2026, 9, 11, 8, 0, "UTC")).unwrap();
        assert_eq!(next.to_zoned(jiff::tz::TimeZone::UTC).date(), date(2026, 9, 14));
        // Friday before seven: the next is today.
        let next = r.next_due(&at(2026, 9, 11, 6, 0, "UTC")).unwrap();
        assert_eq!(next.to_zoned(jiff::tz::TimeZone::UTC).date(), date(2026, 9, 11));
    }

    #[test]
    fn a_trigger_says_when_it_runs_in_words() {
        let seven = time(7, 0, 0, 0);
        let cases = [
            (Trigger::Schedule { at: seven, days: vec![] }, "Every day at 07:00"),
            (
                Trigger::Schedule { at: seven, days: Weekday::WEEKDAYS.to_vec() },
                "Weekdays at 07:00",
            ),
            (
                Trigger::Schedule { at: seven, days: vec![Weekday::Sat, Weekday::Sun] },
                "Weekends at 07:00",
            ),
            (Trigger::Schedule { at: seven, days: vec![Weekday::Wed] }, "Wed at 07:00"),
            (Trigger::TaskDue { lead_days: 1 }, "The day before a task is due"),
            (Trigger::Manual, "Only when you ask"),
        ];
        for (trigger, said) in cases {
            assert_eq!(trigger.describe(), said);
        }
    }

    #[test]
    fn a_day_is_parsed_however_it_was_typed() {
        for (typed, want) in [
            ("mon", Weekday::Mon),
            ("Monday", Weekday::Mon),
            (" TUE ", Weekday::Tue),
            ("saturday", Weekday::Sat),
        ] {
            assert_eq!(Weekday::parse(typed), Some(want), "{typed}");
        }
        assert_eq!(Weekday::parse("someday"), None);
    }

    #[test]
    fn a_routine_needs_a_name_and_something_to_do() {
        let mut r = Routine::new("Brief", "Say what is due.", Trigger::Manual);
        r.validate().expect("a usable routine");
        r.name = "  ".into();
        assert!(r.validate().is_err());
        r.name = "Brief".into();
        r.instructions = String::new();
        assert!(r.validate().is_err(), "a routine with no instructions is an empty prompt");
    }

    #[test]
    fn a_skipped_run_is_finished_and_needs_no_looking_at() {
        let r = morning(&[]);
        let run = RoutineRun::skipped(&r, None, "the vault was locked");
        assert_eq!(run.outcome, Outcome::Skipped);
        assert!(run.outcome.is_finished());
        assert!(run.seen, "there is nothing to look at, so nothing to count on the app bar");
        assert_eq!(run.routine_name, "Morning brief", "a log row reads after a rename");
    }
}

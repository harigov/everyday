//! Medication schedules: what is due when, what answered it, and how
//! steadily.
//!
//! A schedule names local clock times; [`due_times`] turns them into instants
//! for one day in one zone. A logged [`Dose`] answers a due time when it says
//! so (`scheduled_at`) or, failing that, when it is the nearest one inside
//! [`WINDOW_MINUTES`] -- and answers only one, so a single dose never
//! satisfies two due times an hour apart. Everything here is worked out when
//! asked, from the schedule as it is now; nothing is stored.

use super::{Adherence, Dose, DoseStatus, DueDose, MedSchedule, MedStatus, Medication};
use jiff::Timestamp;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use std::collections::{BTreeMap, BTreeSet};

/// A due time is answered by a dose logged within this many minutes either
/// side of it, and is overdue this long after it with nothing logged.
pub const WINDOW_MINUTES: i64 = 120;
/// Taken within this many minutes of the due time counts as on time.
pub const ON_TIME_MINUTES: i64 = 60;

const WINDOW_SECS: i64 = WINDOW_MINUTES * 60;
const ON_TIME_SECS: i64 = ON_TIME_MINUTES * 60;

/// When `medication` is due on local `date` in `tz`, in order. Nothing for
/// a medication that is not active, as needed, not started yet or stopped
/// before `date`.
///
/// A local time that falls in a clock change is read the way a person would:
/// one that does not exist (02:30 on the night the clocks go forward) is
/// taken an hour later, and one that happens twice (01:30 on the night they
/// go back) the first time. Times that collapse onto one instant are one due
/// time.
pub fn due_times(medication: &Medication, date: Date, tz: &TimeZone) -> Vec<Timestamp> {
    // The status says what is true now, not what was true on `date`: a
    // medication stopped last week was still due the week before, and its
    // doses then still count towards adherence. So a status only bounds the
    // dates. A pause has no date of its own; the record's last change stands
    // in for when it began, which is the day the pause was saved unless the
    // record was edited again since.
    let changed = medication.updated_at.to_zoned(tz.clone()).date();
    let last_day = match medication.status {
        MedStatus::Active => medication.stopped,
        MedStatus::Stopped => Some(medication.stopped.unwrap_or(changed)),
        MedStatus::Paused => {
            let before_pause = changed.yesterday().unwrap_or(changed);
            Some(medication.stopped.map_or(before_pause, |stopped| stopped.min(before_pause)))
        }
    };
    if medication.started.is_some_and(|started| date < started)
        || last_day.is_some_and(|last| date > last)
    {
        return Vec::new();
    }
    let mut due: Vec<Timestamp> = times_on(&medication.schedule, date)
        .iter()
        .filter_map(|time| date.to_datetime(*time).to_zoned(tz.clone()).ok())
        .map(|zoned| zoned.timestamp())
        .collect();
    due.sort();
    due.dedup();
    due
}

/// The clock times a schedule names on `date`, none if it is not a day for it.
fn times_on(schedule: &MedSchedule, date: Date) -> &[Time] {
    match schedule {
        MedSchedule::Daily { times } => times,
        MedSchedule::Weekly { days, times } => {
            let weekday = i16::from(date.weekday().to_monday_one_offset());
            if days.iter().any(|day| i16::from(*day) == weekday) { times } else { &[] }
        }
        MedSchedule::EveryNDays { n, from, times } => {
            // A count of zero would divide by it; it names no day at all.
            let counted = (date - *from).get_days();
            if *n > 0 && date >= *from && i64::from(counted) % i64::from(*n) == 0 {
                times
            } else {
                &[]
            }
        }
        MedSchedule::AsNeeded => &[],
    }
}

/// Every due time of every medication on `date`, each with the dose that
/// answered it (by `scheduled_at` first, then the nearest unclaimed dose in
/// the window), overdue when past the window at `now`.
pub fn day_doses(
    medications: &[Medication],
    doses: &[Dose],
    date: Date,
    tz: &TimeZone,
    now: Timestamp,
) -> Vec<DueDose> {
    let mut out = Vec::new();
    for medication in medications {
        for (due_at, dose) in answered_dues(medication, doses, date, date, tz) {
            out.push(DueDose {
                medication_id: medication.id,
                name: medication.name.clone(),
                due_at,
                overdue: dose.is_none() && window_closed(due_at, now),
                dose: dose.cloned(),
            });
        }
    }
    // Stable, so two medications due together stay in the order given.
    out.sort_by_key(|due| due.due_at);
    out
}

/// Taken, skipped and missed against due over local `from..=to`. A due
/// time still inside its window at `now` is not counted yet.
pub fn adherence(
    medications: &[Medication],
    doses: &[Dose],
    from: Date,
    to: Date,
    tz: &TimeZone,
    now: Timestamp,
) -> Adherence {
    let mut out = Adherence::default();
    for medication in medications {
        for (due_at, dose) in answered_dues(medication, doses, from, to, tz) {
            match dose {
                Some(dose) => {
                    out.due += 1;
                    match dose.status {
                        DoseStatus::Skipped => out.skipped += 1,
                        // Never reached, as a missed dose answers nothing
                        // (see `answered_dues`), but it would be a miss.
                        DoseStatus::Missed => out.missed += 1,
                        DoseStatus::Taken => {
                            out.taken += 1;
                            if (dose.at.as_second() - due_at.as_second()).abs() <= ON_TIME_SECS {
                                out.on_time += 1;
                            }
                        }
                    }
                }
                None if window_closed(due_at, now) => {
                    out.due += 1;
                    out.missed += 1;
                }
                None => {}
            }
        }
    }
    out
}

/// Days of supply left at the scheduled rate, if a supply is tracked.
pub fn days_left(medication: &Medication) -> Option<f64> {
    let supply = medication.supply.as_ref()?;
    let used_per_day = doses_per_day(&medication.schedule)? * medication.dose;
    if used_per_day.is_finite() && used_per_day > 0.0 {
        Some((supply.remaining / used_per_day).max(0.0))
    } else {
        None
    }
}

/// Whether the supply is down to its refill reminder.
pub fn needs_refill(medication: &Medication) -> bool {
    match (days_left(medication), &medication.supply) {
        (Some(days), Some(supply)) => days <= f64::from(supply.refill_days),
        _ => false,
    }
}

/// How many takes a day a schedule comes to, on average: a weekly one of
/// three days with one time is three sevenths. `None` when it never comes to
/// any (as needed, or a schedule with no times).
fn doses_per_day(schedule: &MedSchedule) -> Option<f64> {
    let per_day = match schedule {
        MedSchedule::Daily { times } => times.len() as f64,
        MedSchedule::Weekly { days, times } => {
            // The same weekday twice is still one day; a day that is not one
            // never comes.
            let distinct: BTreeSet<u8> =
                days.iter().copied().filter(|day| (1..=7).contains(day)).collect();
            times.len() as f64 * distinct.len() as f64 / 7.0
        }
        MedSchedule::EveryNDays { n, times, .. } => {
            if *n == 0 {
                return None;
            }
            times.len() as f64 / f64::from(*n)
        }
        MedSchedule::AsNeeded => return None,
    };
    (per_day > 0.0).then_some(per_day)
}

fn window_closed(due: Timestamp, now: Timestamp) -> bool {
    now.as_second() > due.as_second() + WINDOW_SECS
}

/// Each due time of `medication` on local `from..=to`, with the dose that
/// answered it.
///
/// The days either side are worked out too, only to compete: a dose at 00:30
/// is as near to the 23:00 of the night before as to a 01:00, and must answer
/// just one of them whichever day was asked for. A dose with status
/// [`DoseStatus::Missed`] is the reminder sweep's note that nothing was
/// taken, which this module works out for itself, so it answers nothing.
fn answered_dues<'a>(
    medication: &Medication,
    doses: &'a [Dose],
    from: Date,
    to: Date,
    tz: &TimeZone,
) -> Vec<(Timestamp, Option<&'a Dose>)> {
    let mut mine: Vec<&Dose> = doses
        .iter()
        .filter(|dose| dose.medication_id == medication.id && dose.status != DoseStatus::Missed)
        .collect();
    mine.sort_by_key(|dose| dose.at);

    // Date order is time order, clock changes included, so these stay sorted.
    let mut dues: Vec<(bool, Timestamp)> = Vec::new();
    let mut day = from.yesterday().unwrap_or(from);
    let last = to.tomorrow().unwrap_or(to);
    while day <= last {
        let asked = day >= from && day <= to;
        dues.extend(due_times(medication, day, tz).into_iter().map(|due| (asked, due)));
        match day.tomorrow() {
            Ok(next) => day = next,
            Err(_) => break,
        }
    }

    let times: Vec<Timestamp> = dues.iter().map(|(_, due)| *due).collect();
    let answered = claim_doses(&times, &mine);
    dues.into_iter()
        .zip(answered)
        .filter(|((asked, _), _)| *asked)
        .map(|((_, due), answer)| (due, answer.map(|i| mine[i])))
        .collect()
}

/// For each of `dues` (sorted), the index into `doses` (sorted by `at`) of the
/// dose that answers it.
///
/// First the doses that name their due time. Then, for the due times left,
/// the closest pairs first: every unclaimed dose within the window of a due
/// time, nearest taken before any further one, each dose and each due time
/// used once. A dose that names a due time that exists is not offered to
/// another; it floats only when the time it names is gone (the schedule was
/// edited since).
fn claim_doses(dues: &[Timestamp], doses: &[&Dose]) -> Vec<Option<usize>> {
    let mut answered: Vec<Option<usize>> = vec![None; dues.len()];
    let mut claimed = vec![false; doses.len()];

    let mut named: BTreeMap<Timestamp, Vec<usize>> = BTreeMap::new();
    for (i, dose) in doses.iter().enumerate() {
        if let Some(scheduled) = dose.scheduled_at {
            named.entry(scheduled).or_default().push(i);
        }
    }
    for (slot, due) in answered.iter_mut().zip(dues) {
        let first = named.get(due).and_then(|names| names.iter().copied().find(|i| !claimed[*i]));
        if let Some(i) = first {
            claimed[i] = true;
            *slot = Some(i);
        }
    }

    let existing: BTreeSet<Timestamp> = dues.iter().copied().collect();
    // (distance in seconds, due index, dose index): sorting puts the nearest
    // pairs first and breaks a tie in favour of the earlier due time.
    let mut pairs: Vec<(i64, usize, usize)> = Vec::new();
    for (d, due) in dues.iter().enumerate() {
        if answered[d].is_some() {
            continue;
        }
        let at = due.as_second();
        let start = doses.partition_point(|dose| dose.at.as_second() < at - WINDOW_SECS);
        for (i, dose) in doses.iter().enumerate().skip(start) {
            if dose.at.as_second() > at + WINDOW_SECS {
                break;
            }
            if claimed[i] || dose.scheduled_at.is_some_and(|name| existing.contains(&name)) {
                continue;
            }
            pairs.push(((dose.at.as_second() - at).abs(), d, i));
        }
    }
    pairs.sort_unstable();
    for (_, d, i) in pairs {
        if answered[d].is_none() && !claimed[i] {
            answered[d] = Some(i);
            claimed[i] = true;
        }
    }
    answered
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Builders the health tests share: a record with every field a test does
    //! not care about filled in.

    use crate::health::{
        Dose, DoseOrigin, DoseStatus, Meal, MealSlot, MedKind, MedSchedule, MedStatus, Medication,
    };
    use crate::id::{DoseId, MealId, MedicationId};
    use jiff::Timestamp;

    pub fn medication(name: &str, schedule: MedSchedule) -> Medication {
        Medication {
            id: MedicationId::new(),
            name: name.to_string(),
            kind: MedKind::default(),
            form: String::new(),
            strength: String::new(),
            dose: 1.0,
            dose_unit: String::new(),
            schedule,
            rules: Vec::new(),
            tags: Vec::new(),
            status: MedStatus::Active,
            started: None,
            stopped: None,
            stop_reason: String::new(),
            history: Vec::new(),
            condition_ids: Vec::new(),
            prescriber: String::new(),
            pharmacy: String::new(),
            supply: None,
            remind: true,
            codes: Vec::new(),
            source_names: Vec::new(),
            notes: String::new(),
            sort_order: 0,
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
        }
    }

    pub fn dose(medication: &Medication, at: Timestamp, status: DoseStatus) -> Dose {
        Dose {
            id: DoseId::new(),
            medication_id: medication.id,
            scheduled_at: None,
            at,
            status,
            amount: None,
            origin: DoseOrigin::Manual,
            note: String::new(),
            key: String::new(),
            hidden: false,
            created_at: at,
            updated_at: at,
        }
    }

    pub fn meal(at: Timestamp) -> Meal {
        Meal {
            id: MealId::new(),
            at,
            offset_min: 0,
            slot: MealSlot::Snack,
            name: String::new(),
            items: Vec::new(),
            photo: None,
            note: String::new(),
            created_at: at,
            updated_at: at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{dose, medication};
    use super::*;
    use crate::health::Supply;
    use jiff::civil::{Weekday, date, time};

    fn la() -> TimeZone {
        TimeZone::get("America/Los_Angeles").unwrap()
    }

    /// The instant of `h:m` local on `day` in `tz`.
    fn at(day: Date, h: i8, m: i8, tz: &TimeZone) -> Timestamp {
        day.at(h, m, 0, 0).to_zoned(tz.clone()).unwrap().timestamp()
    }

    fn utc(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn daily(times: &[(i8, i8)]) -> MedSchedule {
        MedSchedule::Daily { times: times.iter().map(|(h, m)| time(*h, *m, 0, 0)).collect() }
    }

    /// On these weekdays (1 = Monday), once, at `hour`.
    fn weekly(days: &[u8], hour: i8) -> MedSchedule {
        MedSchedule::Weekly { days: days.to_vec(), times: vec![time(hour, 0, 0, 0)] }
    }

    /// Every `n` days from `from`, once, at `hour`.
    fn every_n_days(n: u32, from: Date, hour: i8) -> MedSchedule {
        MedSchedule::EveryNDays { n, from, times: vec![time(hour, 0, 0, 0)] }
    }

    #[test]
    fn a_daily_schedule_is_due_at_each_time_in_order() {
        let tz = la();
        let day = date(2026, 11, 16);
        // Given out of order: the answer is not.
        let med = medication("Metformin", daily(&[(20, 0), (8, 0), (13, 30)]));
        assert_eq!(
            due_times(&med, day, &tz),
            vec![at(day, 8, 0, &tz), at(day, 13, 30, &tz), at(day, 20, 0, &tz)]
        );
    }

    #[test]
    fn nothing_is_due_when_inactive_unstarted_stopped_or_as_needed() {
        let tz = la();
        let day = date(2026, 11, 16);

        let mut med = medication("Metformin", daily(&[(8, 0)]));
        assert_eq!(due_times(&med, day, &tz).len(), 1);

        med.status = MedStatus::Paused;
        assert!(due_times(&med, day, &tz).is_empty());
        med.status = MedStatus::Stopped;
        assert!(due_times(&med, day, &tz).is_empty());

        med.status = MedStatus::Active;
        med.started = Some(date(2026, 11, 17));
        assert!(due_times(&med, day, &tz).is_empty());
        assert_eq!(due_times(&med, date(2026, 11, 17), &tz).len(), 1);

        med.started = None;
        med.stopped = Some(date(2026, 11, 15));
        assert!(due_times(&med, day, &tz).is_empty());
        // The day it stopped is still a day it was taken.
        assert_eq!(due_times(&med, date(2026, 11, 15), &tz).len(), 1);

        let as_needed = medication("Ibuprofen", MedSchedule::AsNeeded);
        assert!(due_times(&as_needed, day, &tz).is_empty());
    }

    #[test]
    fn a_weekly_schedule_is_due_only_on_its_weekdays() {
        let tz = la();
        let monday = date(2026, 11, 16);
        assert_eq!(monday.weekday(), Weekday::Monday);
        // Monday, Wednesday and Sunday (7).
        let med = medication("Methotrexate", weekly(&[1, 3, 7], 9));

        assert_eq!(due_times(&med, monday, &tz), vec![at(monday, 9, 0, &tz)]);
        assert!(due_times(&med, date(2026, 11, 17), &tz).is_empty());
        assert_eq!(due_times(&med, date(2026, 11, 18), &tz).len(), 1);
        assert!(due_times(&med, date(2026, 11, 19), &tz).is_empty());
        assert_eq!(due_times(&med, date(2026, 11, 22), &tz).len(), 1);
    }

    #[test]
    fn an_every_other_day_schedule_counts_from_its_start() {
        let tz = la();
        let med = medication("Injection", every_n_days(2, date(2026, 11, 1), 8));
        assert_eq!(due_times(&med, date(2026, 11, 1), &tz).len(), 1);
        assert!(due_times(&med, date(2026, 11, 2), &tz).is_empty());
        assert_eq!(due_times(&med, date(2026, 11, 3), &tz).len(), 1);
        assert_eq!(due_times(&med, date(2026, 11, 15), &tz).len(), 1);
        assert!(due_times(&med, date(2026, 11, 16), &tz).is_empty());
        // Before it began, whatever the arithmetic would say.
        assert!(due_times(&med, date(2026, 10, 30), &tz).is_empty());
        assert!(due_times(&med, date(2026, 10, 31), &tz).is_empty());

        let never = medication("Nothing", every_n_days(0, date(2026, 11, 1), 8));
        assert!(due_times(&never, date(2026, 11, 1), &tz).is_empty());
    }

    #[test]
    fn clock_changes_move_a_due_time_the_way_a_person_would() {
        let tz = la();

        // Spring forward, Sunday 8 March 2026: 02:30 does not exist and is
        // taken as 03:30 PDT; 09:00 is PDT, so 16:00 UTC.
        let med = medication("Levothyroxine", daily(&[(2, 30), (9, 0)]));
        assert_eq!(
            due_times(&med, date(2026, 3, 8), &tz),
            vec![utc("2026-03-08T10:30:00Z"), utc("2026-03-08T16:00:00Z")]
        );
        // The day before, 09:00 was still PST: 17:00 UTC, an hour later in
        // UTC than the 16:00 of the day the clocks changed.
        assert_eq!(due_times(&med, date(2026, 3, 7), &tz)[1], utc("2026-03-07T17:00:00Z"));

        // Fall back, Sunday 1 November 2026: 01:30 happens twice and is taken
        // the first time (PDT, 08:30 UTC); 09:00 is PST, so 17:00 UTC.
        let med = medication("Levothyroxine", daily(&[(1, 30), (9, 0)]));
        assert_eq!(
            due_times(&med, date(2026, 11, 1), &tz),
            vec![utc("2026-11-01T08:30:00Z"), utc("2026-11-01T17:00:00Z")]
        );

        // Two times that land on one instant are one due time.
        let med = medication("Levothyroxine", daily(&[(2, 30), (3, 30)]));
        assert_eq!(due_times(&med, date(2026, 3, 8), &tz), vec![utc("2026-03-08T10:30:00Z")]);
    }

    #[test]
    fn a_dose_is_matched_by_the_time_it_names_before_any_other() {
        let tz = la();
        let day = date(2026, 11, 16);
        let med = medication("Metformin", daily(&[(8, 0), (9, 0)]));
        // Taken at 08:55, but it says it was the 08:00: the 09:00 is nearer
        // and still does not get it.
        let mut named = dose(&med, at(day, 8, 55, &tz), DoseStatus::Taken);
        named.scheduled_at = Some(at(day, 8, 0, &tz));

        let rows =
            day_doses(std::slice::from_ref(&med), &[named.clone()], day, &tz, at(day, 7, 0, &tz));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].due_at, at(day, 8, 0, &tz));
        assert_eq!(rows[0].dose.as_ref().map(|d| d.id), Some(named.id));
        assert!(rows[1].dose.is_none());
    }

    #[test]
    fn a_dose_with_no_name_answers_the_nearest_due_time_inside_the_window() {
        let tz = la();
        let day = date(2026, 11, 16);
        let med = medication("Metformin", daily(&[(8, 0), (20, 0)]));
        let morning = dose(&med, at(day, 9, 30, &tz), DoseStatus::Taken);
        // Three hours from the evening's: nothing to answer it.
        let stray = dose(&med, at(day, 17, 0, &tz), DoseStatus::Taken);

        let late = at(day, 22, 30, &tz);
        let rows = day_doses(std::slice::from_ref(&med), &[morning.clone(), stray], day, &tz, late);
        assert_eq!(rows[0].dose.as_ref().map(|d| d.id), Some(morning.id));
        assert!(!rows[0].overdue);
        assert!(rows[1].dose.is_none());
        assert!(rows[1].overdue, "22:30 is past the 20:00's window and nothing answered it");
    }

    #[test]
    fn a_dose_is_claimed_by_only_one_due_time() {
        let tz = la();
        let day = date(2026, 11, 16);
        // Two due times an hour apart and one dose between them, nearer the
        // later: it answers that one and leaves the first unanswered.
        let med = medication("Metformin", daily(&[(8, 0), (9, 0)]));
        let one = dose(&med, at(day, 8, 40, &tz), DoseStatus::Taken);

        let rows = day_doses(
            std::slice::from_ref(&med),
            std::slice::from_ref(&one),
            day,
            &tz,
            at(day, 12, 0, &tz),
        );
        assert!(rows[0].dose.is_none());
        assert!(rows[0].overdue, "noon is past the 08:00's window and nothing answered it");
        assert_eq!(rows[1].dose.as_ref().map(|d| d.id), Some(one.id));
        assert!(!rows[1].overdue);

        // Exactly between them: the earlier takes it.
        let between = dose(&med, at(day, 8, 30, &tz), DoseStatus::Taken);
        let rows = day_doses(
            std::slice::from_ref(&med),
            std::slice::from_ref(&between),
            day,
            &tz,
            at(day, 12, 0, &tz),
        );
        assert_eq!(rows[0].dose.as_ref().map(|d| d.id), Some(between.id));
        assert!(rows[1].dose.is_none());

        // And a late-night dose answers the night's due time or the next
        // morning's, never both -- whichever day is asked about.
        let med = medication("Insulin", daily(&[(23, 0), (1, 0)]));
        let night = dose(&med, at(date(2026, 11, 17), 0, 30, &tz), DoseStatus::Taken);
        let after = at(date(2026, 11, 18), 12, 0, &tz);
        let first = day_doses(
            std::slice::from_ref(&med),
            std::slice::from_ref(&night),
            date(2026, 11, 16),
            &tz,
            after,
        );
        let second = day_doses(
            std::slice::from_ref(&med),
            std::slice::from_ref(&night),
            date(2026, 11, 17),
            &tz,
            after,
        );
        let claims: usize = first.iter().chain(&second).filter(|row| row.dose.is_some()).count();
        assert_eq!(claims, 1);
    }

    #[test]
    fn a_missed_note_from_the_sweep_answers_nothing() {
        let tz = la();
        let day = date(2026, 11, 16);
        let med = medication("Metformin", daily(&[(8, 0)]));
        let missed = dose(&med, at(day, 10, 5, &tz), DoseStatus::Missed);

        let rows = day_doses(
            std::slice::from_ref(&med),
            std::slice::from_ref(&missed),
            day,
            &tz,
            at(day, 11, 0, &tz),
        );
        assert!(rows[0].dose.is_none());
        assert!(rows[0].overdue);

        let counts = adherence(&[med], &[missed], day, day, &tz, at(day, 11, 0, &tz));
        assert_eq!(counts, Adherence { due: 1, taken: 0, skipped: 0, missed: 1, on_time: 0 });
    }

    #[test]
    fn the_day_is_every_medications_due_times_by_time() {
        let tz = la();
        let day = date(2026, 11, 16);
        let evening = medication("Statin", daily(&[(21, 0)]));
        let morning = medication("Levothyroxine", daily(&[(6, 30), (21, 0)]));
        let off = medication("Ibuprofen", MedSchedule::AsNeeded);

        let early = at(day, 7, 0, &tz);
        let rows = day_doses(&[evening.clone(), morning.clone(), off], &[], day, &tz, early);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Levothyroxine", "Statin", "Levothyroxine"]);
        assert!(rows.windows(2).all(|pair| pair[0].due_at <= pair[1].due_at));
        assert!(rows.iter().all(|row| !row.overdue));
        assert_eq!(rows[1].medication_id, evening.id);
    }

    #[test]
    fn a_dose_is_overdue_only_after_its_window() {
        let tz = la();
        let day = date(2026, 11, 16);
        let med = medication("Metformin", daily(&[(8, 0)]));
        let meds = [med];
        assert!(!day_doses(&meds, &[], day, &tz, at(day, 10, 0, &tz))[0].overdue);
        assert!(day_doses(&meds, &[], day, &tz, at(day, 10, 1, &tz))[0].overdue);
    }

    #[test]
    fn adherence_counts_taken_skipped_and_missed_but_not_what_is_still_open() {
        let tz = la();
        // The 14th, between them, has nothing logged.
        let (taken_day, skipped_day) = (date(2026, 11, 12), date(2026, 11, 13));
        let today = date(2026, 11, 15);
        let med = medication("Metformin", daily(&[(8, 0)]));
        let doses = [
            dose(&med, at(taken_day, 8, 10, &tz), DoseStatus::Taken),
            dose(&med, at(skipped_day, 8, 0, &tz), DoseStatus::Skipped),
        ];

        // 09:00 on the 15th: that day's 08:00 is inside its window.
        let counts = adherence(
            std::slice::from_ref(&med),
            &doses,
            taken_day,
            today,
            &tz,
            at(today, 9, 0, &tz),
        );
        assert_eq!(counts, Adherence { due: 3, taken: 1, skipped: 1, missed: 1, on_time: 1 });

        // Once the window shuts the 15th is missed too.
        let counts = adherence(&[med], &doses, taken_day, today, &tz, at(today, 10, 1, &tz));
        assert_eq!(counts, Adherence { due: 4, taken: 1, skipped: 1, missed: 2, on_time: 1 });
        assert_eq!(counts.taken + counts.skipped + counts.missed, counts.due);
    }

    #[test]
    fn on_time_is_within_an_hour_either_side() {
        let tz = la();
        let day = date(2026, 11, 16);
        let now = at(date(2026, 11, 17), 0, 0, &tz);
        let med = medication("Metformin", daily(&[(8, 0)]));

        let on_time = |h, m| {
            let doses = [dose(&med, at(day, h, m, &tz), DoseStatus::Taken)];
            adherence(std::slice::from_ref(&med), &doses, day, day, &tz, now)
        };
        assert_eq!(on_time(9, 0).on_time, 1);
        assert_eq!(on_time(7, 0).on_time, 1);
        let late = on_time(9, 1);
        assert_eq!((late.taken, late.on_time), (1, 0));
        let early = on_time(6, 30);
        assert_eq!((early.taken, early.on_time), (1, 0));
    }

    #[test]
    fn a_dose_naming_last_nights_due_time_is_not_counted_for_the_morning() {
        let tz = la();
        let (d1, d2) = (date(2026, 11, 16), date(2026, 11, 17));
        let med = medication("Insulin", daily(&[(23, 0), (1, 0)]));
        // 23:00 on the 16th taken at 00:30 on the 17th, naming what it
        // answered; the 01:00 on the 17th, nearer, has nothing.
        let mut late = dose(&med, at(d2, 0, 30, &tz), DoseStatus::Taken);
        late.scheduled_at = Some(at(d1, 23, 0, &tz));

        let counts = adherence(&[med], &[late], d1, d2, &tz, at(date(2026, 11, 18), 12, 0, &tz));
        // 01:00 and 23:00 on the 16th, 01:00 and 23:00 on the 17th.
        assert_eq!(counts.due, 4);
        assert_eq!(counts.taken, 1);
        assert_eq!(counts.missed, 3);
    }

    fn with_supply(mut med: Medication, remaining: f64, refill_days: u32) -> Medication {
        med.supply = Some(Supply { remaining, refill_days, last_refill: None });
        med
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn days_left_follows_the_scheduled_rate() {
        // Two a day, a take of one and a half: three a day, thirty is ten days.
        let mut med = with_supply(medication("Metformin", daily(&[(8, 0), (20, 0)])), 30.0, 7);
        med.dose = 1.5;
        assert!(close(days_left(&med), 10.0));

        // Three days a week, once: three sevenths a day.
        let thrice = with_supply(medication("Methotrexate", weekly(&[1, 3, 5], 9)), 12.0, 7);
        assert!(close(days_left(&thrice), 28.0));

        // The same weekday twice is one day.
        let twice = with_supply(medication("Methotrexate", weekly(&[1, 1], 9)), 2.0, 7);
        assert!(close(days_left(&twice), 14.0));

        // Every other day: half a dose a day.
        let alternate =
            with_supply(medication("Injection", every_n_days(2, date(2026, 11, 1), 8)), 10.0, 7);
        assert!(close(days_left(&alternate), 20.0));
    }

    #[test]
    fn there_is_no_days_left_without_a_supply_a_rate_or_with_none_left() {
        let no_supply = medication("Metformin", daily(&[(8, 0)]));
        assert_eq!(days_left(&no_supply), None);

        let as_needed = with_supply(medication("Ibuprofen", MedSchedule::AsNeeded), 20.0, 7);
        assert_eq!(days_left(&as_needed), None);

        let no_times = with_supply(medication("Metformin", daily(&[])), 20.0, 7);
        assert_eq!(days_left(&no_times), None);

        let empty = with_supply(medication("Metformin", daily(&[(8, 0)])), 0.0, 7);
        assert!(close(days_left(&empty), 0.0));
    }

    #[test]
    fn a_refill_is_needed_when_the_supply_is_down_to_the_reminder() {
        // Two a day: ten is five days, inside the week's notice.
        let low = with_supply(medication("Metformin", daily(&[(8, 0), (20, 0)])), 10.0, 7);
        assert!(needs_refill(&low));
        // Twenty is ten days.
        let fine = with_supply(medication("Metformin", daily(&[(8, 0), (20, 0)])), 20.0, 7);
        assert!(!needs_refill(&fine));
        // No supply tracked: nothing to remind about.
        assert!(!needs_refill(&medication("Metformin", daily(&[(8, 0)]))));
    }
}

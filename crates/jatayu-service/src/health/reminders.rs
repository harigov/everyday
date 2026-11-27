//! Dose reminders and quiet-source notices, swept every minute by
//! `scheduler::tick` beside the task reminders.
//!
//! A due time is announced once, in the minute it falls due (or the first
//! sweep after, within five minutes -- a machine that slept through a dose
//! does not announce it an hour late). What was announced is remembered in
//! memory for the day; a restart within those five minutes may announce
//! once more, which is the better failure. A Health Auto Export source
//! quiet for longer than `HealthSettings::stale_hours` gets one notice a
//! day.
//!
//! Nothing here is a clinical alarm. A reminder says it is time for a
//! medication the person asked to be reminded of, by the name they gave it;
//! it never says what to take, or that a dose was missed. Names reach the
//! notification and nowhere else -- the log is told how many and what went
//! wrong, never which.

use crate::ctx::Scope;
use crate::events::{Level, Notification};
use crate::service::{Service, blocking};
use jatayu_core::Vault;
use jatayu_core::health::{Connector, DueDose, HealthSource};
use jatayu_core::id::MedicationId;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// How late a due time may be and still be announced.
const WINDOW: SignedDuration = SignedDuration::from_mins(5);

/// How long what was announced is remembered. Longer than the window by far
/// -- an entry only has to outlive the minutes it could be announced again
/// in -- and short enough that a long-running process does not collect them.
const REMEMBER: SignedDuration = SignedDuration::from_hours(24);

/// What has been announced, by notification key, and the time each was for.
static ANNOUNCED: Mutex<Option<HashMap<String, Timestamp>>> = Mutex::new(None);

/// One pass. Never fails the scheduler: what goes wrong is logged.
pub async fn sweep(svc: &Arc<Service>, vault: &Arc<Vault>, now: Timestamp) {
    let looked = blocking({
        let vault = vault.clone();
        move || Ok(look(&vault, now))
    })
    .await;
    let notices = match looked {
        Ok(notices) => notices,
        Err(e) => {
            tracing::warn!(code = %e.code, "could not look for doses and quiet sources");
            return;
        }
    };
    for notice in notices {
        svc.events().notify(notice);
    }
}

/// Everything to say this minute. A failure to look at one thing is logged
/// and does not cost the other.
fn look(vault: &Vault, now: Timestamp) -> Vec<Notification> {
    if !vault.supports_health() {
        return Vec::new();
    }
    let settings = match vault.health_settings() {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!(code = e.code(), "could not read the health settings for reminders");
            return Vec::new();
        }
    };
    let tz = jatayu_core::model::system_tz();
    let zone = TimeZone::get(&tz).unwrap_or(TimeZone::UTC);
    let mut announced = ANNOUNCED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let announced = announced.get_or_insert_with(HashMap::new);
    announced.retain(|_, at| now.duration_since(*at) < REMEMBER);

    let mut notices = Vec::new();
    if settings.dose_reminders {
        match due_doses(vault, now, &tz, &zone) {
            Ok(due) => notices.extend(dose_notices(&due.0, &due.1, now, &zone, announced)),
            Err(e) => tracing::warn!(code = e.code(), "could not look for doses that are due"),
        }
    }
    if settings.stale_hours > 0 {
        match vault.health_sources() {
            Ok(sources) => {
                notices.extend(quiet_notices(&sources, settings.stale_hours, now, &zone, announced))
            }
            Err(e) => tracing::warn!(code = e.code(), "could not look for quiet health sources"),
        }
    }
    notices
}

/// Every due time that could be announced now, with the medications that
/// asked for reminders. Today's, and yesterday's when the window reaches back
/// across midnight in `tz`.
fn due_doses(
    vault: &Vault,
    now: Timestamp,
    tz: &str,
    zone: &TimeZone,
) -> jatayu_core::Result<(Vec<DueDose>, HashSet<MedicationId>)> {
    let today = now.to_zoned(zone.clone()).date();
    let earliest = now.checked_sub(WINDOW).unwrap_or(now).to_zoned(zone.clone()).date();
    let mut due = vault.doses_due(today, tz)?;
    if earliest != today {
        due.extend(vault.doses_due(earliest, tz)?);
    }
    let remind = vault.list_medications()?.into_iter().filter(|m| m.remind).map(|m| m.id).collect();
    Ok((due, remind))
}

/// The reminders to send for `due` right now, each at most once.
///
/// A due time is announced when it is not more than [`WINDOW`] ago, has
/// nothing logged against it, and belongs to a medication that asked for
/// reminders; `announced` is what stops the next minute saying it again.
fn dose_notices(
    due: &[DueDose],
    remind: &HashSet<MedicationId>,
    now: Timestamp,
    zone: &TimeZone,
    announced: &mut HashMap<String, Timestamp>,
) -> Vec<Notification> {
    let mut notices = Vec::new();
    for dose in due {
        let age = now.duration_since(dose.due_at);
        if dose.dose.is_some()
            || !remind.contains(&dose.medication_id)
            || age.is_negative()
            || age > WINDOW
        {
            continue;
        }
        let key = format!("dose:{}:{}", dose.medication_id, dose.due_at.as_second());
        if announced.insert(key.clone(), dose.due_at).is_some() {
            continue;
        }
        let at = dose.due_at.to_zoned(zone.clone()).strftime("%-I:%M %P").to_string();
        notices.push(
            Notification::new(Level::Info, format!("Time for {}", dose.name.trim()))
                .body(format!("Due at {at}"))
                .domain(Scope::Health)
                .for_user()
                .key(key),
        );
    }
    notices
}

/// One notice a day for each Health Auto Export source that has not been
/// heard from for `stale_hours`. A paused source is quiet on purpose and is
/// left alone; one never heard from counts from when it was set up.
fn quiet_notices(
    sources: &[HealthSource],
    stale_hours: u32,
    now: Timestamp,
    zone: &TimeZone,
    announced: &mut HashMap<String, Timestamp>,
) -> Vec<Notification> {
    let after = SignedDuration::from_hours(i64::from(stale_hours));
    let today = now.to_zoned(zone.clone()).date();
    let mut notices = Vec::new();
    for source in sources {
        if source.connector != Connector::HealthAutoExport || source.paused {
            continue;
        }
        let heard = source.status.last_received_at.unwrap_or(source.created_at);
        if now.duration_since(heard) <= after {
            continue;
        }
        let key = format!("health-stale:{}:{today}", source.id);
        if announced.insert(key.clone(), now).is_some() {
            continue;
        }
        let since = heard.to_zoned(zone.clone()).strftime("%a %-d %b, %-I:%M %P").to_string();
        notices.push(
            Notification::new(Level::Info, format!("No health data from your phone since {since}"))
                .body(format!(
                    "{} has sent nothing since. Health Auto Export may be paused, or the vault \
                     was locked when it tried.",
                    source.name.trim()
                ))
                .domain(Scope::Health)
                .for_user()
                .key(key),
        );
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::health::{Dose, SourceStatus};

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn due(medication: MedicationId, name: &str, due_at: Timestamp) -> DueDose {
        DueDose { medication_id: medication, name: name.into(), due_at, dose: None, overdue: false }
    }

    fn asked(ids: &[MedicationId]) -> HashSet<MedicationId> {
        ids.iter().copied().collect()
    }

    fn source(connector: Connector, last: Option<Timestamp>, created: Timestamp) -> HealthSource {
        HealthSource {
            id: jatayu_core::id::HealthSourceId::new(),
            connector,
            name: "Phone".into(),
            paused: false,
            status: SourceStatus { last_received_at: last, ..SourceStatus::default() },
            token_id: String::new(),
            created_at: created,
            updated_at: created,
        }
    }

    #[test]
    fn a_dose_falling_due_this_minute_is_announced_by_name_and_keyed() {
        let medication = MedicationId::new();
        let now = at("2026-11-15T08:00:30Z");
        let due_at = at("2026-11-15T08:00:00Z");
        let mut announced = HashMap::new();
        let notices = dose_notices(
            &[due(medication, "Levothyroxine", due_at)],
            &asked(&[medication]),
            now,
            &TimeZone::UTC,
            &mut announced,
        );
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Time for Levothyroxine");
        assert_eq!(notices[0].body.as_deref(), Some("Due at 8:00 am"));
        assert_eq!(
            notices[0].key,
            Some(format!("dose:{medication}:{}", due_at.as_second())),
            "keyed by the medication and the due time"
        );
    }

    #[test]
    fn a_dose_is_announced_once_however_many_minutes_it_stays_in_the_window() {
        let medication = MedicationId::new();
        let due_at = at("2026-11-15T08:00:00Z");
        let mut announced = HashMap::new();
        let list = [due(medication, "Metformin", due_at)];
        let remind = asked(&[medication]);
        let mut sweep =
            |now: &str| dose_notices(&list, &remind, at(now), &TimeZone::UTC, &mut announced).len();
        let counts = (sweep("2026-11-15T08:01:00Z"), sweep("2026-11-15T08:02:00Z"));
        assert_eq!(counts, (1, 0));
    }

    #[test]
    fn a_dose_not_yet_due_or_more_than_five_minutes_late_is_left_alone() {
        let medication = MedicationId::new();
        let due_at = at("2026-11-15T08:00:00Z");
        let list = [due(medication, "Metformin", due_at)];
        let remind = asked(&[medication]);
        for now in ["2026-11-15T07:59:59Z", "2026-11-15T08:05:01Z", "2026-11-15T09:00:00Z"] {
            let mut announced = HashMap::new();
            let notices = dose_notices(&list, &remind, at(now), &TimeZone::UTC, &mut announced);
            assert!(notices.is_empty(), "announced at {now}");
            assert!(announced.is_empty(), "remembered what it did not say at {now}");
        }
    }

    #[test]
    fn a_dose_already_taken_or_not_asked_about_is_not_announced() {
        let (asked_for, silent) = (MedicationId::new(), MedicationId::new());
        let due_at = at("2026-11-15T08:00:00Z");
        let mut taken = due(asked_for, "Metformin", due_at);
        let now = at("2026-11-15T08:01:00Z");
        taken.dose = Some(
            serde_json::from_value::<Dose>(serde_json::json!({
                "id": jatayu_core::id::DoseId::new(),
                "medicationId": asked_for,
                "at": now,
                "createdAt": now,
                "updatedAt": now,
            }))
            .unwrap(),
        );
        let untaken_but_silent = due(silent, "Vitamin D", due_at);
        let mut announced = HashMap::new();
        let notices = dose_notices(
            &[taken, untaken_but_silent],
            &asked(&[asked_for]),
            now,
            &TimeZone::UTC,
            &mut announced,
        );
        assert!(notices.is_empty(), "{:?}", notices.iter().map(|n| &n.title).collect::<Vec<_>>());
    }

    #[test]
    fn a_dose_is_shown_at_the_local_time_it_was_for() {
        let medication = MedicationId::new();
        let zone = TimeZone::get("America/Los_Angeles").unwrap();
        // 16:00 UTC is 8:00 in Los Angeles in November.
        let due_at = at("2026-11-15T16:00:00Z");
        let mut announced = HashMap::new();
        let notices = dose_notices(
            &[due(medication, "Levothyroxine", due_at)],
            &asked(&[medication]),
            at("2026-11-15T16:00:10Z"),
            &zone,
            &mut announced,
        );
        assert_eq!(notices[0].body.as_deref(), Some("Due at 8:00 am"));
    }

    #[test]
    fn a_source_quiet_past_the_limit_is_said_once_a_day() {
        let now = at("2026-11-15T12:00:00Z");
        let heard = Some(at("2026-11-14T06:00:00Z"));
        let quiet = source(Connector::HealthAutoExport, heard, at("2026-10-01T00:00:00Z"));
        let list = std::slice::from_ref(&quiet);
        let mut announced = HashMap::new();
        let first = quiet_notices(list, 24, now, &TimeZone::UTC, &mut announced);
        assert_eq!(first.len(), 1);
        let title = &first[0].title;
        assert!(title.starts_with("No health data from your phone since"), "{title}");
        assert_eq!(first[0].key, Some(format!("health-stale:{}:2026-11-15", quiet.id)));
        let evening = at("2026-11-15T18:00:00Z");
        let later = quiet_notices(list, 24, evening, &TimeZone::UTC, &mut announced);
        assert!(later.is_empty(), "said again the same day");
        let tomorrow = at("2026-11-16T12:00:00Z");
        let next_day = quiet_notices(list, 24, tomorrow, &TimeZone::UTC, &mut announced);
        assert_eq!(next_day.len(), 1, "a new day is a new notice");
    }

    #[test]
    fn a_source_heard_from_lately_paused_or_not_a_phone_is_not_quiet() {
        let now = at("2026-11-15T12:00:00Z");
        let long_ago = at("2026-10-01T00:00:00Z");
        let this_morning = Some(at("2026-11-15T06:00:00Z"));
        let recent = source(Connector::HealthAutoExport, this_morning, long_ago);
        let mut paused = source(Connector::HealthAutoExport, Some(long_ago), long_ago);
        paused.paused = true;
        let export = source(Connector::AppleExport, Some(long_ago), long_ago);
        let manual = source(Connector::Manual, None, long_ago);
        let mut announced = HashMap::new();
        let list = [recent, paused, export, manual];
        let notices = quiet_notices(&list, 24, now, &TimeZone::UTC, &mut announced);
        assert!(notices.is_empty());
    }

    #[test]
    fn a_source_never_heard_from_counts_from_when_it_was_set_up() {
        let now = at("2026-11-15T12:00:00Z");
        let mut announced = HashMap::new();
        let fresh = source(Connector::HealthAutoExport, None, at("2026-11-15T06:00:00Z"));
        assert!(quiet_notices(&[fresh], 24, now, &TimeZone::UTC, &mut announced).is_empty());
        let old = source(Connector::HealthAutoExport, None, at("2026-11-10T06:00:00Z"));
        assert_eq!(quiet_notices(&[old], 24, now, &TimeZone::UTC, &mut announced).len(), 1);
    }
}

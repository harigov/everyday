//! Trackers and their readings.
//!
//! Both halves are vault records. A tracker used to be a field inside one
//! journal's sealed payload, which made "what am I tracking" a setting of that
//! journal and a habit something that belonged to one -- so `list_trackers` is
//! also where the one migration that cannot be a SQL step runs.

use crate::command;
use crate::ctx::{Ctx, Scope};
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use jatayu_core::model::{local_date_in, system_tz};
use jatayu_core::store::trackers::{ReadingQuery, TrackerDay};
use jatayu_core::tracker::{Reading, Tracker, TrackerKind, TrackerSource};
use jatayu_core::{EntryId, JournalId, ReadingId, TrackerId};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

use super::Nothing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTracker {
    pub name: String,
    pub kind: TrackerKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Readings {
    pub query: ReadingQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTracker {
    pub tracker: Tracker,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerRef {
    pub id: TrackerId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeTrackers {
    pub from: TrackerId,
    pub into: TrackerId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogReading {
    pub tracker_id: TrackerId,
    pub value: f64,
    pub date: jiff::civil::Date,
    #[serde(default)]
    pub at: Option<jiff::Timestamp>,
    /// Both optional: a reading logged from the Overview, or from the tray,
    /// was ticked on no page at all.
    #[serde(default)]
    pub journal_id: Option<JournalId>,
    #[serde(default)]
    pub entry_id: Option<EntryId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveReading {
    pub reading: Reading,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingRef {
    pub id: ReadingId,
}

// ---- trackers that count what a scope guards ---------------------------
//
// A tracker is a Trackers record, so the Trackers scope opens every one of
// them -- but one whose source is the Health app is a day's heart rate, and
// one whose source is Screen time is which sites somebody spent their day on.
// A token narrowed to Trackers (or one that holds everything except those)
// must not read either through here what it cannot read through `health` or
// `screen_time`, nor make a tracker that would. Held to the scope of the
// domain the figures come from, not to a rule of its own.

/// Is a tracker counting `source` one `ctx` may not see: it reads Health and
/// the caller does not hold [`Scope::Health`], or it reads Screen time and
/// the caller does not hold [`Scope::ScreenTime`]?
fn out_of_reach(ctx: &Ctx, source: &TrackerSource) -> bool {
    (source.reads_health() && !ctx.holds(Scope::Health))
        || (source.reads_screen_time() && !ctx.holds(Scope::ScreenTime))
}

/// The refusal for naming such a source, in the words every other command
/// refuses a scope in.
fn refuse_out_of_reach(ctx: &Ctx, source: &TrackerSource) -> CommandResult<()> {
    if !out_of_reach(ctx, source) {
        return Ok(());
    }
    let scope = if source.reads_health() { Scope::Health } else { Scope::ScreenTime };
    Err(CommandError::new(
        codes::FORBIDDEN,
        format!("this connection is not allowed to reach {}", scope.as_str()),
    ))
}

/// The ids of every stored tracker `ctx` may not see, for dropping the days
/// and readings that name a tracker by its id alone. Empty -- and the
/// trackers not listed at all -- for a caller that holds both scopes, which
/// is every window and paired copy of this application.
fn ids_out_of_reach(
    ctx: &Ctx,
    vault: &jatayu_core::Vault,
) -> jatayu_core::Result<HashSet<TrackerId>> {
    if ctx.holds(Scope::Health) && ctx.holds(Scope::ScreenTime) {
        return Ok(HashSet::new());
    }
    Ok(vault
        .trackers()?
        .into_iter()
        .filter(|t| out_of_reach(ctx, &t.source))
        .map(|t| t.id)
        .collect())
}

/// Mint a tracker, without saving it.
///
/// The id, the timestamps and the defaults come from here for the same reason
/// `new_journal` mints a journal: `crypto.randomUUID` needs a secure context
/// the packaged webview does not always provide, and a tracker that silently
/// fails to get an id is a tracker whose readings all pile up under the same
/// one.
async fn new_tracker(svc: Arc<Service>, _ctx: Ctx, args: NewTracker) -> CommandResult<Tracker> {
    let _ = svc.require()?;
    let mut tracker = Tracker::new(args.name, args.kind);
    tracker.normalize();
    Ok(tracker)
}

/// Every tracker in the vault, moving any that a pre-v7 vault still keeps
/// inside its journals on the way.
///
/// The move is here, in the first call the feature makes, rather than in
/// `unlock`, and for the reason the library's seeding is in `list_kinds`: a
/// vault whose owner never opens the journal never pays for it, and putting
/// work on the unlock path makes every unlock slower for a thing that happens
/// once. It runs at most once per vault -- see
/// `Vault::migrate_journal_trackers`, which is idempotent by construction.
async fn list_trackers(svc: Arc<Service>, ctx: Ctx, _args: Nothing) -> CommandResult<Vec<Tracker>> {
    let vault = svc.require()?;
    blocking(move || {
        if let Err(e) = vault.migrate_journal_trackers() {
            // Not fatal. An unwritable vault cannot be migrated and can still
            // be read; what it loses is the old definitions, which is a strip
            // with no chips rather than an error over the window.
            tracing::warn!(error = %e, "could not move the journals' trackers");
        }
        Ok(vault.trackers()?.into_iter().filter(|t| !out_of_reach(&ctx, &t.source)).collect())
    })
    .await
}

/// Both sources are held to the scope: the one being saved, so a Health
/// tracker cannot be made by a caller that cannot read Health, and the one
/// already stored under this id, so a caller that cannot see a Health tracker
/// cannot rewrite it -- into a manual one, say, to read its name and targets
/// back -- either.
async fn save_tracker(svc: Arc<Service>, ctx: Ctx, args: SaveTracker) -> CommandResult<()> {
    refuse_out_of_reach(&ctx, &args.tracker.source)?;
    let id = args.tracker.id;
    if let Some(stored) = svc.on_vault(move |vault| Ok(vault.tracker(id).ok())).await? {
        refuse_out_of_reach(&ctx, &stored.source)?;
    }
    svc.on_vault(move |vault| vault.save_tracker(&args.tracker)).await
}

/// Fold one tracker into another, keeping both histories, and answer how many
/// readings moved.
async fn merge_trackers(svc: Arc<Service>, _ctx: Ctx, args: MergeTrackers) -> CommandResult<u64> {
    svc.on_vault(move |vault| vault.merge_trackers(args.from, args.into)).await
}

async fn list_readings(svc: Arc<Service>, ctx: Ctx, args: Readings) -> CommandResult<Vec<Reading>> {
    svc.on_vault(move |vault| {
        let hidden = ids_out_of_reach(&ctx, vault)?;
        let mut readings = vault.readings(&args.query)?;
        readings.retain(|r| !hidden.contains(&r.tracker_id));
        Ok(readings)
    })
    .await
}

/// One row per tracker per day: the aggregate every chart is built from.
///
/// The days of a tracker `ctx` may not see are dropped from the answer rather
/// than left out of the query, so a tracker since deleted keeps its days.
async fn tracker_days(
    svc: Arc<Service>,
    ctx: Ctx,
    args: Readings,
) -> CommandResult<Vec<TrackerDay>> {
    svc.on_vault(move |vault| {
        let hidden = ids_out_of_reach(&ctx, vault)?;
        let mut days = vault.tracker_days(&args.query)?;
        days.retain(|d| !hidden.contains(&d.tracker_id));
        Ok(days)
    })
    .await
}

/// Record one value, and decide what "when" means.
///
/// The whole of that decision lives here, in one place, because it is the
/// question this domain is easiest to get quietly wrong:
///
/// * an explicit `at` is always believed -- the person corrected the time;
/// * ticking something on **today's** page records the minute, because that
///   minute is real: you are logging it as it happens;
/// * ticking something on a **past** page records the day and no minute at all.
///   Writing up Tuesday on Thursday says something true about Tuesday and
///   nothing whatever about 23:04, and a defaulted timestamp there would put a
///   mark on the calendar at an hour nothing happened.
async fn log_reading(svc: Arc<Service>, _ctx: Ctx, args: LogReading) -> CommandResult<Reading> {
    let vault = svc.require()?;
    let now = svc.now();
    blocking(move || {
        let tz = system_tz();
        let at = match args.at {
            Some(at) => Some(at),
            None if args.date == local_date_in(now, &tz) => Some(now),
            None => None,
        };

        let mut reading = match at {
            Some(at) => Reading::at(args.tracker_id, at, &tz, args.value),
            None => Reading::on(args.tracker_id, args.date, args.value),
        };
        reading.tz = tz;
        reading.journal_id = args.journal_id;
        reading.entry_id = args.entry_id;
        vault.save_reading(&reading)?;
        // Read back rather than returned as written: the vault clamps the value
        // against the tracker's definition, and a caller should draw what was
        // stored rather than what it asked for.
        Ok(vault.reading(reading.id)?)
    })
    .await
}

/// Update a reading that already exists: a corrected dose, a note, a time.
async fn save_reading(svc: Arc<Service>, _ctx: Ctx, args: SaveReading) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_reading(&args.reading)).await
}

async fn delete_reading(svc: Arc<Service>, _ctx: Ctx, args: ReadingRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_reading(args.id)).await
}

/// Delete a tracker along with every reading it ever made, returning how many
/// went. Archiving is the non-destructive half and is a `save_tracker` with the
/// flag set.
async fn delete_tracker(svc: Arc<Service>, ctx: Ctx, args: TrackerRef) -> CommandResult<u64> {
    let id = args.id;
    if let Some(stored) = svc.on_vault(move |vault| Ok(vault.tracker(id).ok())).await? {
        refuse_out_of_reach(&ctx, &stored.source)?;
    }
    svc.on_vault(move |vault| vault.delete_tracker(args.id)).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_trackers", scope: Trackers, effect: Read,
        args: Nothing, returns: "Tracker[]", signature: &[],
        run: list_trackers,
    },
    command! {
        name: "save_tracker", scope: Trackers, effect: Write,
        change: Tracker / Updated,
        id: |a: &SaveTracker| Some(a.tracker.id.to_string()),
        args: SaveTracker, returns: "void",
        signature: &[("tracker", "Tracker", true)],
        run: save_tracker,
    },
    command! {
        name: "merge_trackers", scope: Trackers, effect: Destructive,
        change: Tracker / Deleted,
        args: MergeTrackers, returns: "number",
        signature: &[("from", "TrackerId", true), ("into", "TrackerId", true)],
        run: merge_trackers,
    },
    command! {
        name: "new_tracker", scope: Trackers, effect: Read,
        args: NewTracker, returns: "Tracker",
        signature: &[("name", "string", true), ("kind", "TrackerKind", true)],
        run: new_tracker,
    },
    command! {
        name: "list_readings", scope: Trackers, effect: Read,
        args: Readings, returns: "Reading[]",
        signature: &[("query", "ReadingQuery", true)],
        run: list_readings,
    },
    command! {
        name: "tracker_days", scope: Trackers, effect: Read,
        args: Readings, returns: "TrackerDay[]",
        signature: &[("query", "ReadingQuery", true)],
        run: tracker_days,
    },
    command! {
        name: "log_reading", scope: Trackers, effect: Write,
        change: Reading / Created,
        args: LogReading, returns: "Reading",
        signature: &[
            ("trackerId", "TrackerId", true),
            ("value", "number", true),
            ("date", "string", true),
            ("at", "string | null", false),
            ("journalId", "JournalId | null", false),
            ("entryId", "EntryId | null", false),
        ],
        run: log_reading,
    },
    command! {
        name: "save_reading", scope: Trackers, effect: Write,
        change: Reading / Updated,
        id: |a: &SaveReading| Some(a.reading.id.to_string()),
        args: SaveReading, returns: "void",
        signature: &[("reading", "Reading", true)],
        run: save_reading,
    },
    command! {
        name: "delete_reading", scope: Trackers, effect: Destructive,
        change: Reading / Deleted,
        id: |a: &ReadingRef| Some(a.id.to_string()),
        args: ReadingRef, returns: "void",
        signature: &[("id", "ReadingId", true)],
        run: delete_reading,
    },
    command! {
        name: "delete_tracker", scope: Trackers, effect: Destructive,
        change: Tracker / Deleted,
        id: |a: &TrackerRef| Some(a.id.to_string()),
        args: TrackerRef, returns: "number",
        signature: &[("id", "TrackerId", true)],
        run: delete_tracker,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Caller;
    use jatayu_core::health::{Metric, Stat};

    fn holding(scopes: Vec<Scope>) -> Ctx {
        Ctx { caller: Caller::Local, scopes, proved_at: None, request_id: None }
    }

    fn health() -> TrackerSource {
        TrackerSource::Health { metric: Metric::Steps, stat: Stat::Auto }
    }

    fn screen() -> TrackerSource {
        TrackerSource::ScreenTime { category: None, item: None }
    }

    #[test]
    fn a_tracker_counting_health_or_screen_time_needs_the_scope_of_what_it_counts() {
        let trackers_only = holding(vec![Scope::Trackers]);
        assert!(out_of_reach(&trackers_only, &health()));
        assert!(out_of_reach(&trackers_only, &screen()));
        // Everything else a tracker can count is the Trackers scope's own.
        for source in
            [TrackerSource::Manual, TrackerSource::Time, TrackerSource::Finished { kind_id: None }]
        {
            assert!(!out_of_reach(&trackers_only, &source), "{source:?}");
        }

        let with_health = holding(vec![Scope::Trackers, Scope::Health]);
        assert!(!out_of_reach(&with_health, &health()));
        assert!(out_of_reach(&with_health, &screen()), "Health is not Screen time");

        let with_screen = holding(vec![Scope::Trackers, Scope::ScreenTime]);
        assert!(out_of_reach(&with_screen, &health()));
        assert!(!out_of_reach(&with_screen, &screen()));

        // `All` is the owner's window and every paired copy of it.
        assert!(!out_of_reach(&Ctx::local(), &health()) && !out_of_reach(&Ctx::local(), &screen()));
    }

    #[test]
    fn naming_a_source_out_of_reach_is_refused_as_every_other_scope_is() {
        let trackers_only = holding(vec![Scope::Trackers]);
        let err = refuse_out_of_reach(&trackers_only, &health()).unwrap_err();
        assert_eq!(err.code, codes::FORBIDDEN);
        assert!(err.message.contains("health"), "{}", err.message);
        let err = refuse_out_of_reach(&trackers_only, &screen()).unwrap_err();
        assert!(err.message.contains("screentime"), "{}", err.message);
        assert!(refuse_out_of_reach(&trackers_only, &TrackerSource::Manual).is_ok());
        assert!(refuse_out_of_reach(&Ctx::local(), &health()).is_ok());
    }
}

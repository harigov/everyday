//! Trackers and their readings.
//!
//! Both halves are vault records. A tracker used to be a field inside one
//! journal's sealed payload, which made "what am I tracking" a setting of that
//! journal and a habit something that belonged to one -- so `list_trackers` is
//! also where the one migration that cannot be a SQL step runs.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use everyday_core::model::{local_date_in, system_tz};
use everyday_core::store::trackers::{ReadingQuery, TrackerDay};
use everyday_core::tracker::{Reading, Tracker, TrackerKind};
use everyday_core::{EntryId, JournalId, ReadingId, TrackerId};
use serde::Deserialize;
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
async fn list_trackers(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Tracker>> {
    let vault = svc.require()?;
    blocking(move || {
        if let Err(e) = vault.migrate_journal_trackers() {
            // Not fatal. An unwritable vault cannot be migrated and can still
            // be read; what it loses is the old definitions, which is a strip
            // with no chips rather than an error over the window.
            tracing::warn!(error = %e, "could not move the journals' trackers");
        }
        Ok(vault.trackers()?)
    })
    .await
}

async fn save_tracker(svc: Arc<Service>, _ctx: Ctx, args: SaveTracker) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_tracker(&args.tracker)?)).await
}

/// Fold one tracker into another, keeping both histories, and answer how many
/// readings moved.
async fn merge_trackers(svc: Arc<Service>, _ctx: Ctx, args: MergeTrackers) -> CommandResult<u64> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.merge_trackers(args.from, args.into)?)).await
}

async fn list_readings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Readings,
) -> CommandResult<Vec<Reading>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.readings(&args.query)?)).await
}

/// One row per tracker per day: the aggregate every chart is built from.
async fn tracker_days(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Readings,
) -> CommandResult<Vec<TrackerDay>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.tracker_days(&args.query)?)).await
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
    blocking(move || {
        let tz = system_tz();
        let now = jiff::Timestamp::now();
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
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_reading(&args.reading)?)).await
}

async fn delete_reading(svc: Arc<Service>, _ctx: Ctx, args: ReadingRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_reading(args.id)?)).await
}

/// Delete a tracker along with every reading it ever made, returning how many
/// went. Archiving is the non-destructive half and is a `save_tracker` with the
/// flag set.
async fn delete_tracker(svc: Arc<Service>, _ctx: Ctx, args: TrackerRef) -> CommandResult<u64> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_tracker(args.id)?)).await
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
        args: SaveReading, returns: "void",
        signature: &[("reading", "Reading", true)],
        run: save_reading,
    },
    command! {
        name: "delete_reading", scope: Trackers, effect: Destructive,
        change: Reading / Deleted,
        args: ReadingRef, returns: "void",
        signature: &[("id", "ReadingId", true)],
        run: delete_reading,
    },
    command! {
        name: "delete_tracker", scope: Trackers, effect: Destructive,
        change: Tracker / Deleted,
        args: TrackerRef, returns: "number",
        signature: &[("id", "TrackerId", true)],
        run: delete_tracker,
    },
];

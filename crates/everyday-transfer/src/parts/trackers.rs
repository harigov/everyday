//! Tracking, as a column of numbers per thing tracked.
//!
//! ```text
//!   trackers/
//!     trackers.csv     what is being tracked, and what a reading means
//!     Weight.csv       date, time, value, note
//!     Ibuprofen.csv
//! ```
//!
//! A reading is a date and a number. That is a time series, and a time series
//! belongs in a column -- one file per tracker, so `Weight.csv` opens in
//! anything that draws a graph without first having to be filtered. A single
//! `readings.csv` with a tracker column would be the database's shape rather
//! than the question's.
//!
//! The definitions are separate because they are few and they are different
//! in kind: what a tracker *is* -- a habit with a cadence, a dose with a unit,
//! a scale out of five -- is a handful of rows that change about never, beside
//! thousands that change daily. That split is the same one the vault makes,
//! where a tracker is a sealed record and a reading is a row in the clear.

use super::doc;
use crate::text::{Csv, Table, safe_name};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec};
use everyday_core::store::JournalStore;
use everyday_core::store::trackers::{ReadingQuery, TrackerStore};
use everyday_core::tracker::{Cadence, Period, Reading, Tracker, TrackerKind};
use everyday_core::{JournalId, ReadingId, Result, TrackerId};
use std::collections::BTreeMap;

pub struct TrackersPart;
pub static TRACKERS: TrackersPart = TrackersPart;

static SPEC: Spec = Spec {
    id: "trackers",
    label: "Tracking",
    summary: "What your days produced in numbers rather than in prose: habits, doses, \
              weights, counts — every reading, with the day and minute it was taken.",
    format: "CSV, one file per tracker",
    media: false,
    imports: true,
};

const INDEX: &str = "trackers.csv";
const INDEX_COLUMNS: &[&str] = &[
    "name",
    "file",
    "kind",
    "unit",
    "icon",
    "color",
    "default_value",
    "target",
    "scale_max",
    "cadence",
    "on_calendar",
    "purpose",
    "archived",
    "order",
    "id",
];
const READING_COLUMNS: &[&str] = &["date", "time", "value", "unit", "note", "timezone", "id"];

impl Portable for TrackersPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(trackers) = store.trackers() else { return Ok(None) };
        Ok(Some(trackers.list_readings(&ReadingQuery::default())?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(trackers) = store.trackers() else { return Ok(()) };
        let definitions = trackers.list_trackers()?;
        let readings = trackers.list_readings(&ReadingQuery::default())?;

        let mut index = Csv::new(INDEX_COLUMNS);
        let mut taken: Vec<String> = Vec::new();
        for tracker in &definitions {
            let mut stem = safe_name(&tracker.name);
            while taken.contains(&stem) {
                stem = format!("{stem}-{}", tracker.id.short());
            }
            taken.push(stem.clone());
            let file = format!("{stem}.csv");

            index.row(&[
                tracker.name.clone(),
                file.clone(),
                kind_name(tracker.kind).to_string(),
                tracker.unit.clone(),
                tracker.icon.clone(),
                tracker.color.clone(),
                tracker.default_value.to_string(),
                tracker.target.map(|t| t.to_string()).unwrap_or_default(),
                tracker.scale_max.to_string(),
                tracker
                    .cadence
                    .as_ref()
                    .map(|c| format!("{} per {}", c.times, period_name(c.per)))
                    .unwrap_or_default(),
                tracker.on_calendar.to_string(),
                doc::purpose_text(tracker.purpose.as_ref()),
                tracker.archived.to_string(),
                tracker.sort_order.to_string(),
                tracker.id.to_string(),
            ]);

            let mut csv = Csv::new(READING_COLUMNS);
            let mut mine: Vec<&Reading> =
                readings.iter().filter(|r| r.tracker_id == tracker.id).collect();
            mine.sort_by_key(|r| (r.local_date, r.at));
            for reading in &mine {
                csv.row(&[
                    reading.local_date.to_string(),
                    // The minute, where there was one. A tracker without a
                    // clock -- "did I meditate today" -- has readings whose
                    // time is genuinely unknown, and an invented midnight
                    // would be a fact nobody recorded.
                    reading
                        .at
                        .map(|at| {
                            let z = at.to_zoned(zone(&reading.tz));
                            format!("{:02}:{:02}", z.hour(), z.minute())
                        })
                        .unwrap_or_default(),
                    reading.value.to_string(),
                    tracker.unit.clone(),
                    reading.note.clone(),
                    reading.tz.clone(),
                    reading.id.to_string(),
                ]);
            }
            out.records(&file, csv.finish(), mine.len() as u64)?;
        }
        if index.rows() > 0 {
            out.text(INDEX, index.finish())?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(trackers) = store.trackers() else {
            report.problem(SPEC.label, "this vault's backend does not track anything");
            return Ok(report);
        };

        let mut by_file: BTreeMap<String, TrackerId> = BTreeMap::new();
        if let Some(text) = src.text(INDEX) {
            for row in Table::parse(text).rows() {
                match read_tracker(trackers, &row, mode) {
                    Ok((id, existed)) => {
                        by_file.insert(row.get("file").to_string(), id);
                        report.count(existed, mode);
                    }
                    Err(e) => report.problem(INDEX, e),
                }
            }
        }

        // A journal for readings that name one. Readings do not require it --
        // the field is optional -- so this stays `None` rather than inventing
        // a journal nobody asked for.
        let journal: Option<JournalId> = store.list_journals()?.first().map(|j| j.id);

        for (name, text) in src.documents(".csv") {
            if name == INDEX {
                continue;
            }
            let table = Table::parse(text);
            if !table.has("value") {
                report.problem(name, "no `value` column, so this is not a tracker");
                continue;
            }
            let tracker = match by_file.get(name) {
                Some(id) => *id,
                None => match tracker_named(trackers, name.trim_end_matches(".csv"), &table) {
                    Ok(id) => {
                        by_file.insert(name.to_string(), id);
                        id
                    }
                    Err(e) => {
                        report.problem(name, e);
                        continue;
                    }
                },
            };
            for row in table.rows() {
                match read_reading(trackers, &row, tracker, journal, mode) {
                    Ok(existed) => report.count(existed, mode),
                    Err(e) => report.problem(name, e),
                }
            }
        }
        Ok(report)
    }
}

fn read_tracker(
    trackers: &dyn TrackerStore,
    row: &crate::text::Row<'_>,
    mode: Mode,
) -> Result<(TrackerId, bool)> {
    let id = TrackerId::parse(row.get("id")).unwrap_or_else(|_| TrackerId::new());
    let existing = trackers.get_tracker(id).ok();
    if existing.is_some() && mode == Mode::Skip {
        return Ok((id, true));
    }
    let mut tracker = existing.clone().unwrap_or_else(|| {
        Tracker::new(row.get("name"), kind(row.get("kind")).unwrap_or_default())
    });
    tracker.id = id;
    tracker.name = row.get("name").to_string();
    tracker.kind = kind(row.get("kind")).unwrap_or(tracker.kind);
    tracker.unit = row.get("unit").to_string();
    if !row.get("icon").is_empty() {
        tracker.icon = row.get("icon").to_string();
    }
    if !row.get("color").is_empty() {
        tracker.color = row.get("color").to_string();
    }
    if let Some(value) = row.parse("default_value") {
        tracker.default_value = value;
    }
    tracker.target = row.parse("target");
    if let Some(max) = row.parse("scale_max") {
        tracker.scale_max = max;
    }
    tracker.cadence = cadence(row.get("cadence"));
    tracker.on_calendar = row.flag("on_calendar");
    tracker.purpose = doc::parse_purpose(row.get("purpose"));
    tracker.archived = row.flag("archived");
    if let Some(order) = row.parse("order") {
        tracker.sort_order = order;
    }
    tracker.updated_at = jiff::Timestamp::now();
    trackers.put_tracker(&tracker)?;
    Ok((id, existing.is_some()))
}

/// The tracker a file belongs to when no index named one.
fn tracker_named(trackers: &dyn TrackerStore, name: &str, table: &Table) -> Result<TrackerId> {
    let name = name.replace('-', " ");
    if let Some(tracker) =
        trackers.list_trackers()?.into_iter().find(|t| t.name.eq_ignore_ascii_case(&name))
    {
        return Ok(tracker.id);
    }
    let mut tracker = Tracker::new(&name, TrackerKind::Check);
    // The unit is the same on every row of a tracker's file, so the first row
    // that states one states the tracker's.
    if let Some(unit) = table.rows().map(|r| r.get("unit").to_string()).find(|u| !u.is_empty()) {
        tracker.unit = unit;
        tracker.kind = TrackerKind::Amount;
    }
    trackers.put_tracker(&tracker)?;
    Ok(tracker.id)
}

fn read_reading(
    trackers: &dyn TrackerStore,
    row: &crate::text::Row<'_>,
    tracker_id: TrackerId,
    journal_id: Option<JournalId>,
    mode: Mode,
) -> Result<bool> {
    let id = ReadingId::parse(row.get("id")).unwrap_or_else(|_| ReadingId::new());
    let existing = trackers.get_reading(id).ok();
    if existing.is_some() && mode == Mode::Skip {
        return Ok(true);
    }
    let existed = existing.is_some();
    let date: jiff::civil::Date = row
        .parse("date")
        .ok_or_else(|| everyday_core::Error::Invalid("a reading has no date".into()))?;
    let value: f64 = row
        .parse("value")
        .ok_or_else(|| everyday_core::Error::Invalid("a reading has no number".into()))?;

    let tz = match row.get("timezone") {
        "" => "UTC".to_string(),
        tz => tz.to_string(),
    };
    let mut reading = existing.unwrap_or_else(|| Reading::on(tracker_id, date, value));
    reading.id = id;
    reading.tracker_id = tracker_id;
    reading.journal_id = reading.journal_id.or(journal_id);
    reading.local_date = date;
    reading.value = value;
    reading.tz = tz;
    reading.note = row.get("note").to_string();
    reading.at = clock(row.get("time"), date, &reading.tz);
    reading.updated_at = jiff::Timestamp::now();
    trackers.put_reading(&reading)?;
    Ok(existed)
}

/// `08:15` on `date` in `tz`, as an instant. `None` for a reading whose time
/// was never known.
fn clock(text: &str, date: jiff::civil::Date, tz: &str) -> Option<jiff::Timestamp> {
    let (h, m) = text.trim().split_once(':')?;
    let time = jiff::civil::Time::new(h.parse().ok()?, m.parse().ok()?, 0, 0).ok()?;
    date.to_datetime(time).to_zoned(zone(tz)).ok().map(|z| z.timestamp())
}

fn zone(tz: &str) -> jiff::tz::TimeZone {
    jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC)
}

fn kind_name(kind: TrackerKind) -> &'static str {
    match kind {
        TrackerKind::Check => "check",
        TrackerKind::Dose => "dose",
        TrackerKind::Scale => "scale",
        TrackerKind::Amount => "amount",
    }
}

fn kind(name: &str) -> Option<TrackerKind> {
    match name.trim().to_ascii_lowercase().as_str() {
        "check" => Some(TrackerKind::Check),
        "dose" => Some(TrackerKind::Dose),
        "scale" => Some(TrackerKind::Scale),
        "amount" => Some(TrackerKind::Amount),
        _ => None,
    }
}

fn period_name(period: Period) -> &'static str {
    match period {
        Period::Day => "day",
        Period::Week => "week",
        Period::Month => "month",
    }
}

/// `3 per week`, as it is written in the file.
fn cadence(text: &str) -> Option<Cadence> {
    let (times, per) = text.trim().split_once(" per ")?;
    Some(Cadence {
        times: times.trim().parse().ok()?,
        per: match per.trim() {
            "week" => Period::Week,
            "month" => Period::Month,
            _ => Period::Day,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cadence_reads_the_way_it_is_written() {
        let c = Cadence { times: 3, per: Period::Week };
        let text = format!("{} per {}", c.times, period_name(c.per));
        let back = cadence(&text).unwrap();
        assert_eq!((back.times, back.per), (3, Period::Week));
        assert!(cadence("").is_none());
    }

    #[test]
    fn a_reading_with_no_clock_keeps_none_rather_than_midnight() {
        assert!(clock("", jiff::civil::date(2026, 9, 10), "UTC").is_none());
        assert!(clock("08:15", jiff::civil::date(2026, 9, 10), "UTC").is_some());
    }
}

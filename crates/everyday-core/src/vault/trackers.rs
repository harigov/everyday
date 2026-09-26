//! Trackers and the readings they produce.
//!
//! The seventh domain, and the one that used to be split across two
//! layers: the definitions rode along inside a `Journal` while the
//! readings they produced lived in a store of their own. Both are records
//! now, and what is left in the journal is which chips it draws.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{ItemId, KindId, ReadingId, TrackerId};
use crate::purpose::Purpose;
use crate::record::RecordKind;
use crate::store::library::LogQuery;
use crate::store::purpose::{GoalQuery, PurposeWindow};
use crate::store::trackers::{ReadingQuery, TrackerDay, TrackerStore};
use crate::timestamped::Timestamped;
use crate::tracker::{Reading, Target, TargetProgress, Tracker, TrackerSource};
use jiff::civil::Date;
use std::collections::BTreeMap;

impl Vault {
    /// Does this vault's backend store trackers at all?
    pub fn supports_trackers(&self) -> bool {
        self.with_trackers(|_| Ok(())).is_ok()
    }

    fn with_trackers<T>(&self, f: impl FnOnce(&dyn TrackerStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Trackers, |s| s.trackers().map(f))
    }

    pub fn trackers(&self) -> Result<Vec<Tracker>> {
        self.with_trackers(|t| {
            let mut all = t.list_trackers()?;
            all.sort_by_key(|t| (t.sort_order, t.created_at));
            Ok(all)
        })
    }

    pub fn tracker(&self, id: TrackerId) -> Result<Tracker> {
        self.with_trackers(|t| t.get_tracker(id))
    }

    pub fn save_tracker(&self, tracker: &Tracker) -> Result<()> {
        self.writable()?;
        let mut tracker = tracker.clone();
        tracker.normalize();
        if tracker.name.is_empty() {
            return Err(Error::Invalid("a tracker needs a name".into()));
        }
        tracker.touch();
        let id = tracker.id;
        self.with_trackers(|t| t.put_tracker(&tracker))?;
        self.wrote(RecordKind::Tracker, id);
        Ok(())
    }

    /// Delete a tracker and every reading it ever made.
    ///
    /// The destructive half of a pair. Archiving — a flag on the definition
    /// — is the other and the usual one: it takes the tracker off the page
    /// and keeps its history, which is what "I stopped taking this in March"
    /// actually means. This is for the tracker added by mistake, and it says
    /// so by taking the readings too rather than leaving numbers behind that
    /// nothing can name.
    ///
    /// Every journal that drew a chip for it is left alone. A stale id in
    /// `shown_trackers` is skipped when the strip is built, and rewriting
    /// six journals to tidy one list is a great deal of writing to avoid an
    /// `if let`.
    pub fn delete_tracker(&self, id: TrackerId) -> Result<u64> {
        self.writable()?;
        let count = self.with_trackers(|t| t.delete_tracker(id))?;
        self.wrote(RecordKind::Tracker, id);
        Ok(count)
    }

    /// Fold one tracker into another, keeping both histories.
    ///
    /// The tidy-up lazy creation needs: `#swim` on Monday and `#swimming` on
    /// Friday are two records of the same thing, and the answer cannot be to
    /// throw away a month of numbers. Every journal showing the old one is
    /// switched to the new, because that *is* one list per journal and the
    /// alternative is a chip that silently stops drawing.
    pub fn merge_trackers(&self, from: TrackerId, into: TrackerId) -> Result<u64> {
        self.writable()?;
        if from == into {
            return Err(Error::Invalid("a tracker cannot be merged into itself".into()));
        }
        // Both must exist before anything moves: merging into a tracker that
        // is not there would strand every reading under an id nothing names.
        // And both must be recorded by hand: a derived tracker's readings are
        // not rows to move, and rows moved onto one would never be read.
        let (a, b) = self.with_trackers(|t| Ok((t.get_tracker(from)?, t.get_tracker(into)?)))?;
        refuse_derived(&a)?;
        refuse_derived(&b)?;
        let moved = self.with_trackers(|t| t.merge_trackers(from, into))?;
        // `from` is what the store's own merge removes; `into` is written
        // too (its reading count changes) but its own edit is not one this
        // vault method has a fresh copy of to name beyond its id.
        self.wrote(RecordKind::Tracker, from);
        self.wrote(RecordKind::Tracker, into);
        for mut journal in self.journals()? {
            if !journal.shows(from) {
                continue;
            }
            journal.hide(from);
            journal.show(into);
            self.save_journal(&journal)?;
        }
        Ok(moved)
    }

    /// Every reading the query matches, the derived ones included.
    ///
    /// A derived tracker's readings are one per day it has anything, made up
    /// on the spot from its [`derived_days`](Vault::derived_days). They carry
    /// an id that is stable for the tracker and the day, so a list that is
    /// asked for twice draws the same rows, but no such reading is stored and
    /// asking for it by id finds nothing.
    pub fn readings(&self, query: &ReadingQuery) -> Result<Vec<Reading>> {
        let stored = self.with_trackers(|t| t.list_readings(query))?;
        let derived = self.derived_days(query, None)?;
        if derived.is_empty() {
            return Ok(stored);
        }
        let mut all = stored;
        all.extend(derived.iter().map(derived_reading));
        Ok(query.apply(all))
    }

    pub fn reading(&self, id: ReadingId) -> Result<Reading> {
        self.with_trackers(|t| t.get_reading(id))
    }

    /// One row per tracker per day. What every chart is built from, and so
    /// where a derived tracker's numbers join everyone else's.
    pub fn tracker_days(&self, query: &ReadingQuery) -> Result<Vec<TrackerDay>> {
        self.tracker_days_among(query, None)
    }

    /// [`tracker_days`](Vault::tracker_days), for a caller that already holds
    /// the tracker definitions and should not have them listed again.
    fn tracker_days_among(
        &self,
        query: &ReadingQuery,
        trackers: Option<&[Tracker]>,
    ) -> Result<Vec<TrackerDay>> {
        let mut days = self.with_trackers(|t| t.tracker_days(query))?;
        let derived = self.derived_days(query, trackers)?;
        if !derived.is_empty() {
            days.extend(derived);
            days.sort_by_key(|d| (d.date, d.tracker_id));
        }
        Ok(days)
    }

    /// Every target of every given tracker, measured over its current period.
    ///
    /// One `tracker_days` query, reaching back to the start of the longest
    /// period any of them asks about, rather than one per tracker. Trackers
    /// with no targets are left out of the answer. Weeks start on Monday;
    /// see [`Period::start_of`](crate::tracker::Period::start_of).
    pub fn target_progress(&self, trackers: &[Tracker], today: Date) -> Result<Vec<Measured>> {
        let measured: Vec<&Tracker> = trackers.iter().filter(|t| !t.targets.is_empty()).collect();
        let Some(from) =
            measured.iter().flat_map(|t| &t.targets).map(|g| g.per.start_of(today)).min()
        else {
            return Ok(Vec::new());
        };
        let query = ReadingQuery {
            tracker_ids: measured.iter().map(|t| t.id).collect(),
            ..ReadingQuery::between(from, today)
        };
        let days = self.tracker_days_among(&query, Some(trackers))?;
        Ok(measured
            .into_iter()
            .map(|t| {
                let each = t.targets.iter().map(|g| (*g, t.progress(g, &days, today))).collect();
                (t.clone(), each)
            })
            .collect())
    }

    /// The days a derived tracker has, over whatever the query asks for.
    ///
    /// Computed rather than stored, from records the vault already keeps:
    /// time blocks for [`TrackerSource::Time`], the library's log for
    /// [`TrackerSource::Finished`]. One grouped query per source however
    /// many trackers share it, so a page with five of them costs two scans.
    ///
    /// Nothing derived was ticked on a journal's page, beside an entry, or
    /// at a known minute, so a query that asks for any of those gets none --
    /// and neither does one that asked for recorded readings only.
    /// A backend without the domain a source reads from answers nothing for
    /// it rather than failing the whole query: the manual readings beside
    /// them still have to draw.
    fn derived_days(
        &self,
        query: &ReadingQuery,
        known: Option<&[Tracker]>,
    ) -> Result<Vec<TrackerDay>> {
        if query.recorded_only
            || query.journal_id.is_some()
            || query.entry_id.is_some()
            || query.timed_only
        {
            return Ok(Vec::new());
        }
        let listed;
        let all = match known {
            Some(trackers) => trackers,
            None => {
                listed = self.trackers()?;
                &listed
            }
        };
        let derived: Vec<Tracker> = all
            .iter()
            .filter(|t| !t.is_manual())
            .filter(|t| query.tracker_ids.is_empty() || query.tracker_ids.contains(&t.id))
            .cloned()
            .collect();
        if derived.is_empty() {
            return Ok(Vec::new());
        }
        // An open-ended query still needs a window to hand the backend. These
        // bounds are wider than any record a person makes, and are strings
        // both databases compare in date order.
        let window = PurposeWindow::new(
            query.from.unwrap_or(Date::constant(1900, 1, 1)),
            query.to.unwrap_or(Date::constant(2200, 12, 31)),
        );

        let mut out = Vec::new();
        let timed: Vec<&Tracker> =
            derived.iter().filter(|t| t.source == TrackerSource::Time).collect();
        if !timed.is_empty() && self.supports_purpose() {
            out.extend(self.time_days(&timed, window)?);
        }
        let finished: Vec<&Tracker> =
            derived.iter().filter(|t| matches!(t.source, TrackerSource::Finished { .. })).collect();
        if !finished.is_empty() && self.supports_library() {
            out.extend(self.finished_days(&finished, window)?);
        }
        Ok(out)
    }

    /// Minutes of actual time per day, for each tracker's own purpose.
    ///
    /// A tracker filed under a goal counts that goal's time. One filed under
    /// a role counts the role's and every one of its goals', because "an
    /// hour a day for myself" is met by an hour on any of the things being
    /// yourself is made of. A tracker filed under nothing counts nothing:
    /// "time on" needs something for the time to be on.
    fn time_days(&self, trackers: &[&Tracker], window: PurposeWindow) -> Result<Vec<TrackerDay>> {
        let rows = self.actual_minutes_by_day(window)?;
        let goals = self.goals(&GoalQuery::default())?;
        let counts = |mine: &Purpose, row: &Purpose| match mine {
            Purpose::Goal { .. } => row == mine,
            Purpose::Role { id } => {
                row == mine
                    || row.goal_id().is_some_and(|g| {
                        goals.iter().any(|goal| goal.id == g && goal.role_id == *id)
                    })
            }
        };

        let mut out = Vec::new();
        for tracker in trackers {
            let Some(mine) = &tracker.purpose else { continue };
            let mut by_day: BTreeMap<Date, u64> = BTreeMap::new();
            for row in &rows {
                if row.purpose.as_ref().is_some_and(|p| counts(mine, p)) {
                    *by_day.entry(row.date).or_default() += row.minutes;
                }
            }
            out.extend(
                by_day
                    .into_iter()
                    .filter(|(_, m)| *m > 0)
                    .map(|(date, m)| derived_day(tracker.id, date, m as f64)),
            );
        }
        Ok(out)
    }

    /// Things got to the end of, per day, for each tracker's shelf.
    ///
    /// Counted from the library's log rather than from each item's own
    /// finish date, so a re-read is a second book read that year — which is
    /// what "twelve books this year" is counting.
    fn finished_days(
        &self,
        trackers: &[&Tracker],
        window: PurposeWindow,
    ) -> Result<Vec<TrackerDay>> {
        let logs = self.logs(&LogQuery::completions(window.from, window.to))?;
        // The shelf each logged item sits on, looked up once per distinct
        // item finished in the window and shared by every tracker. Per item
        // rather than by listing a shelf: a year's finishes are a dozen or a
        // few hundred, a shelf can be thousands, and listing it would open
        // every one of them.
        //
        // A log naming an item that is not there is skipped -- it cannot say
        // which shelf it counted towards. The SQL backends make that state
        // impossible with a foreign key; a backend that does not is not
        // allowed to fail the whole count over it. Any other failure is the
        // answer, not a book quietly left out of the count.
        let mut shelves: BTreeMap<ItemId, Option<KindId>> = BTreeMap::new();
        let mut out = Vec::new();
        for tracker in trackers {
            let TrackerSource::Finished { kind_id } = tracker.source else { continue };
            let mut by_day: BTreeMap<Date, u64> = BTreeMap::new();
            for log in &logs {
                if let Some(wanted) = kind_id {
                    let shelf = match shelves.get(&log.item_id) {
                        Some(shelf) => *shelf,
                        None => {
                            let shelf = match self.item(log.item_id) {
                                Ok(item) => Some(item.kind_id),
                                Err(Error::NotFound { .. }) => None,
                                Err(e) => return Err(e),
                            };
                            shelves.insert(log.item_id, shelf);
                            shelf
                        }
                    };
                    if shelf != Some(wanted) {
                        continue;
                    }
                }
                *by_day.entry(log.date).or_default() += 1;
            }
            out.extend(by_day.into_iter().map(|(date, n)| derived_day(tracker.id, date, n as f64)));
        }
        Ok(out)
    }

    /// Write a reading, having first made it mean something.
    ///
    /// The value is clamped by the *definition* — a severity cannot be 40 on
    /// a scale of ten, and a check is one or zero however the caller wrote
    /// it — because the alternative is a chart with an axis to the moon and
    /// no way to tell which of a thousand rows caused it. Looking the
    /// tracker up is also how a reading naming one that does not exist is
    /// refused here rather than becoming an unnameable row.
    pub fn save_reading(&self, reading: &Reading) -> Result<()> {
        self.writable()?;
        let tracker = self.tracker(reading.tracker_id)?;
        refuse_derived(&tracker)?;

        let mut reading = reading.clone();
        reading.value = tracker.clamp(reading.value);
        reading.note = reading.note.trim().to_string();
        reading.touch();
        // A reading's day and its instant have to agree, or a chip logged at
        // 00:10 lands on the calendar a day away from the entry it was
        // ticked under. The instant wins: it is the more precise of the two.
        if let Some(at) = reading.at {
            reading.local_date = crate::model::local_date_in(at, &reading.tz);
        }
        let id = reading.id;
        self.with_trackers(|t| t.put_reading(&reading))?;
        self.wrote(RecordKind::Reading, id);
        Ok(())
    }

    pub fn delete_reading(&self, id: ReadingId) -> Result<()> {
        self.writable()?;
        self.with_trackers(|t| t.delete_reading(id))?;
        self.wrote(RecordKind::Reading, id);
        Ok(())
    }

    /// Move the tracker definitions a pre-v7 vault kept inside its journals
    /// into the trackers table, once.
    ///
    /// This is the one migration in the application that cannot be a SQL
    /// step, and the reason is the whole design working as intended: the
    /// definitions are inside a sealed journal payload, and no migration
    /// running against the database can read a word of it. Only something
    /// holding the data key can, which means it happens here, on unlock,
    /// inside the write claim.
    ///
    /// Idempotent by construction. A journal's old list is cleared as it is
    /// moved, so a second run finds nothing to do; and a tracker whose id is
    /// already in the table is skipped rather than overwritten, so a vault
    /// interrupted half way through does not lose the edits made to the half
    /// that landed.
    ///
    /// Returns how many definitions were moved.
    pub fn migrate_journal_trackers(&self) -> Result<usize> {
        if !self.supports_trackers() || !self.is_writable() {
            return Ok(0);
        }
        let journals = self.journals()?;
        if journals.iter().all(|j| j.trackers.is_empty()) {
            return Ok(0);
        }
        let existing: Vec<TrackerId> =
            self.with_trackers(|t| t.list_trackers())?.into_iter().map(|t| t.id).collect();

        let mut moved = 0;
        for mut journal in journals {
            if journal.trackers.is_empty() {
                continue;
            }
            for tracker in std::mem::take(&mut journal.trackers) {
                // A journal that drew a chip for it keeps drawing one: the
                // move must be invisible, and the strip is the only thing
                // anybody would notice.
                journal.show(tracker.id);
                if existing.contains(&tracker.id) {
                    continue;
                }
                self.with_trackers(|t| t.put_tracker(&tracker))?;
                moved += 1;
            }
            journal.touch();
            self.save_journal(&journal)?;
        }
        Ok(moved)
    }
}

/// One tracker, and each of its targets measured over its current period.
pub type Measured = (Tracker, Vec<(Target, TargetProgress)>);

/// Refuse to write a reading against a tracker whose readings are computed.
fn refuse_derived(tracker: &Tracker) -> Result<()> {
    if tracker.is_manual() {
        return Ok(());
    }
    Err(Error::Invalid(format!(
        "\u{201c}{}\u{201d} is worked out from what the vault already records, and cannot be \
         recorded by hand",
        tracker.name
    )))
}

fn derived_day(tracker_id: TrackerId, date: Date, value: f64) -> TrackerDay {
    TrackerDay { tracker_id, date, count: 1, sum: value, max: value, first_at: None, last_at: None }
}

/// A derived day as a reading, with an id that is the same every time.
///
/// Mixed from the tracker's id and the day so that the same list asked for
/// twice has the same keys -- an interface keying rows by id would otherwise
/// redraw every one of them on every refresh.
fn derived_reading(day: &TrackerDay) -> Reading {
    let serial = day.date.since(Date::constant(1970, 1, 1)).map(|s| s.get_days()).unwrap_or(0);
    let mixed = day.tracker_id.0.as_u128() ^ (serial as u128).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut reading = Reading::on(day.tracker_id, day.date, day.sum);
    reading.id = ReadingId(uuid::Uuid::from_u128(mixed));
    reading
}

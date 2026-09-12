//! Trackers and the readings they produce.
//!
//! The seventh domain, and the one that used to be split across two
//! layers: the definitions rode along inside a `Journal` while the
//! readings they produced lived in a store of their own. Both are records
//! now, and what is left in the journal is which chips it draws.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{ReadingId, TrackerId};
use crate::store::trackers::{ReadingQuery, TrackerDay, TrackerStore};
use crate::tracker::{Reading, Tracker};
use jiff::Timestamp;

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
        tracker.updated_at = Timestamp::now();
        self.with_trackers(|t| t.put_tracker(&tracker))
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
        self.with_trackers(|t| t.delete_tracker(id))
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
        self.with_trackers(|t| {
            t.get_tracker(from)?;
            t.get_tracker(into)
        })?;
        let moved = self.with_trackers(|t| t.merge_trackers(from, into))?;
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

    pub fn readings(&self, query: &ReadingQuery) -> Result<Vec<Reading>> {
        self.with_trackers(|t| t.list_readings(query))
    }

    pub fn reading(&self, id: ReadingId) -> Result<Reading> {
        self.with_trackers(|t| t.get_reading(id))
    }

    /// One row per tracker per day. What every chart is built from.
    pub fn tracker_days(&self, query: &ReadingQuery) -> Result<Vec<TrackerDay>> {
        self.with_trackers(|t| t.tracker_days(query))
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

        let mut reading = reading.clone();
        reading.value = tracker.clamp(reading.value);
        reading.note = reading.note.trim().to_string();
        reading.updated_at = Timestamp::now();
        // A reading's day and its instant have to agree, or a chip logged at
        // 00:10 lands on the calendar a day away from the entry it was
        // ticked under. The instant wins: it is the more precise of the two.
        if let Some(at) = reading.at {
            reading.local_date = crate::model::local_date_in(at, &reading.tz);
        }
        self.with_trackers(|t| t.put_reading(&reading))
    }

    pub fn delete_reading(&self, id: ReadingId) -> Result<()> {
        self.writable()?;
        self.with_trackers(|t| t.delete_reading(id))
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
            journal.updated_at = Timestamp::now();
            self.save_journal(&journal)?;
        }
        Ok(moved)
    }
}

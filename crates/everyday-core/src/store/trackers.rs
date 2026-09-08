//! Storage for the tracking domain: the readings a journal's trackers
//! produced.
//!
//! # Why this is a fourth trait
//!
//! The same reason [`TaskStore`](super::tasks::TaskStore) is a second one and
//! [`CalendarStore`](super::calendars::CalendarStore) a third. A backend that
//! stores journals as a tree of Markdown files has no good answer for tens of
//! thousands of timestamped numbers, and folding this into
//! [`JournalStore`](super::JournalStore) would oblige it to invent one. So the
//! tracking domain is reached through
//! [`JournalStore::trackers`](super::JournalStore::trackers), which returns
//! `None` by default, and the interface reads
//! [`Capabilities::trackers`](super::Capabilities::trackers) to know whether
//! to offer tracking at all rather than discovering it from an error at click
//! time.
//!
//! Note what is *not* here: the trackers themselves. A
//! [`Tracker`](crate::tracker::Tracker) is a setting of a
//! [`Journal`](crate::model::Journal) and is saved with it, by every backend,
//! encrypted like the rest of that record. This trait owns exactly the part
//! that needs a table: the readings.
//!
//! # The aggregate is part of the contract
//!
//! [`TrackerStore::tracker_days`] exists because the alternative is worse.
//! "Average severity per day for the last year" is a `GROUP BY` a database
//! does in one pass over an index; done in the application it is a thousand
//! decryptions to produce thirty numbers. Making it a method means the SQLite
//! backend can answer it in SQL, and a backend that cannot still answers
//! correctly through [`ReadingQuery::totals`], which is the same arithmetic
//! written once.

use crate::error::Result;
use crate::id::{EntryId, JournalId, ReadingId, TrackerId};
use crate::tracker::{Aggregate, Reading};
use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Filter for [`TrackerStore::list_readings`].
///
/// All filters are ANDed. An empty query matches every reading in the vault.
/// This is the shape both callers need: the entry's chip row asks for one
/// journal on one day, and the calendar asks for one week.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReadingQuery {
    /// Restrict to one journal. `None` means every journal — which is what
    /// the calendar wants, since it draws the vault rather than a selection.
    pub journal_id: Option<JournalId>,
    /// Restrict to these trackers. Empty means any.
    pub tracker_ids: Vec<TrackerId>,
    /// Restrict to readings recorded beside one entry.
    pub entry_id: Option<EntryId>,
    /// Inclusive bounds on `local_date`.
    pub from: Option<Date>,
    pub to: Option<Date>,
    /// Keep only readings that know their time of day. What an
    /// hour-of-day question has to ask, and what stops an unknown minute
    /// being averaged in as though it were midnight.
    pub timed_only: bool,
    /// Cap on how many readings [`TrackerStore::list_readings`] returns.
    /// `None` means no limit.
    ///
    /// Deliberately not honoured by [`TrackerStore::tracker_days`]. A cap on
    /// an aggregate has no meaning worth agreeing on -- truncate the rows
    /// first and every total is silently partial, truncate the groups
    /// instead and it means something else again -- so the aggregate ignores
    /// it and covers the whole window the other filters describe.
    pub limit: Option<u32>,
}

impl ReadingQuery {
    /// Everything on the days from `from` to `to` inclusive.
    pub fn between(from: Date, to: Date) -> Self {
        Self { from: Some(from), to: Some(to), ..Default::default() }
    }

    /// One journal, one day. The entry's chip row.
    pub fn on_day(journal_id: JournalId, date: Date) -> Self {
        Self {
            journal_id: Some(journal_id),
            from: Some(date),
            to: Some(date),
            ..Default::default()
        }
    }

    pub fn matches(&self, r: &Reading) -> bool {
        if let Some(j) = self.journal_id
            && r.journal_id != j
        {
            return false;
        }
        if !self.tracker_ids.is_empty() && !self.tracker_ids.contains(&r.tracker_id) {
            return false;
        }
        if self.entry_id.is_some() && r.entry_id != self.entry_id {
            return false;
        }
        if let Some(from) = self.from
            && r.local_date < from
        {
            return false;
        }
        if let Some(to) = self.to
            && r.local_date > to
        {
            return false;
        }
        if self.timed_only && r.at.is_none() {
            return false;
        }
        true
    }

    /// Filter, order and truncate a fully-materialised list.
    ///
    /// Ordered by day, then by time within the day, with the untimed
    /// readings of a day first — they are the ones that mean "sometime
    /// today", and a list that runs 08:00, *unknown*, 19:00 reads as though
    /// the unknown one happened at lunchtime.
    pub fn apply(&self, mut rows: Vec<Reading>) -> Vec<Reading> {
        rows.retain(|r| self.matches(r));
        rows.sort_by(|a, b| {
            a.local_date.cmp(&b.local_date).then(a.at.cmp(&b.at)).then(a.id.cmp(&b.id))
        });
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }

    /// Roll a materialised list up into one [`TrackerDay`] per tracker per
    /// day. The fallback behind [`TrackerStore::tracker_days`], and the
    /// definition of what that method must return.
    ///
    /// Filters but does not paginate: see [`ReadingQuery::limit`] for why an
    /// aggregate ignores it. Nor does it need the ordering `apply` imposes
    /// -- every field here is order-independent -- so this walks the rows
    /// once instead of sorting them first.
    pub fn totals(&self, rows: Vec<Reading>) -> Vec<TrackerDay> {
        let mut acc: BTreeMap<(Date, TrackerId), TrackerDay> = BTreeMap::new();
        for r in rows.into_iter().filter(|r| self.matches(r)) {
            let day = acc.entry((r.local_date, r.tracker_id)).or_insert_with(|| TrackerDay {
                tracker_id: r.tracker_id,
                date: r.local_date,
                count: 0,
                sum: 0.0,
                max: f64::NEG_INFINITY,
                first_at: None,
                last_at: None,
            });
            day.count += 1;
            day.sum += r.value;
            day.max = day.max.max(r.value);
            if let Some(at) = r.at {
                day.first_at = Some(day.first_at.map_or(at, |f: Timestamp| f.min(at)));
                day.last_at = Some(day.last_at.map_or(at, |l: Timestamp| l.max(at)));
            }
        }
        acc.into_values()
            .map(|mut d| {
                if !d.max.is_finite() {
                    d.max = 0.0;
                }
                d
            })
            .collect()
    }
}

/// One tracker's day, rolled up. The unit an analytics view is built from.
///
/// Deliberately raw: `count` and `sum` rather than "the answer", because
/// which of them *is* the answer depends on the tracker's
/// [`Aggregate`](crate::tracker::Aggregate) and that belongs to the
/// definition, not to the arithmetic. [`TrackerDay::value_for`] applies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerDay {
    pub tracker_id: TrackerId,
    pub date: Date,
    /// How many readings landed on this day.
    pub count: u32,
    pub sum: f64,
    pub max: f64,
    /// Earliest and latest *known* times. `None` when every reading that day
    /// was recorded without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<Timestamp>,
}

impl TrackerDay {
    /// The one number this day is worth, under a given aggregate.
    pub fn value_for(&self, aggregate: Aggregate) -> f64 {
        match aggregate {
            Aggregate::Count => f64::from(self.count),
            Aggregate::Sum => self.sum,
            Aggregate::Mean => {
                if self.count == 0 {
                    0.0
                } else {
                    self.sum / f64::from(self.count)
                }
            }
        }
    }
}

/// The persistence contract for readings. See the module docs.
pub trait TrackerStore: Send + Sync {
    fn list_readings(&self, query: &ReadingQuery) -> Result<Vec<Reading>>;

    fn get_reading(&self, id: ReadingId) -> Result<Reading>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_reading(&self, reading: &Reading) -> Result<()>;

    fn delete_reading(&self, id: ReadingId) -> Result<()>;

    /// Delete every reading of one tracker, returning how many went.
    ///
    /// What "delete this tracker, and its history" means. Archiving is the
    /// other answer and the usual one; this is here because a tracker added
    /// by mistake should be able to leave without a trace.
    fn delete_readings_of(&self, tracker: TrackerId) -> Result<u64>;

    /// Delete every reading in one journal. Part of the journal cascade: a
    /// deleted journal takes its entries, and it has to take these too or
    /// the next tracker to be created inherits a stranger's history.
    fn delete_readings_in(&self, journal: JournalId) -> Result<u64>;

    /// One row per tracker per day over the queried window.
    ///
    /// Every filter on the query applies except [`ReadingQuery::limit`],
    /// which is about lists rather than aggregates and is ignored here by
    /// both this default and the backends that override it.
    ///
    /// The default is correct and reads everything; a backend that can say
    /// it in SQL should override it, and the SQLite one does.
    fn tracker_days(&self, query: &ReadingQuery) -> Result<Vec<TrackerDay>> {
        Ok(query.totals(self.list_readings(query)?))
    }
}

/// Associated data binding a reading's ciphertext to its row. See
/// [`entry_aad`](super::entry_aad).
pub fn reading_aad(id: ReadingId) -> Vec<u8> {
    format!("everyday.reading.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::Reading;

    fn day(n: i8) -> Date {
        Date::constant(2026, 3, n)
    }

    fn reading(journal: JournalId, tracker: TrackerId, d: i8, value: f64) -> Reading {
        Reading::on(journal, tracker, day(d), value)
    }

    #[test]
    fn totals_split_by_tracker_and_by_day() {
        let j = JournalId::new();
        let (a, b) = (TrackerId::new(), TrackerId::new());
        let rows = vec![
            reading(j, a, 1, 400.0),
            reading(j, a, 1, 400.0),
            reading(j, a, 2, 200.0),
            reading(j, b, 1, 3.0),
        ];
        let totals = ReadingQuery::default().totals(rows);
        assert_eq!(totals.len(), 3);

        let first = totals.iter().find(|t| t.tracker_id == a && t.date == day(1)).unwrap();
        assert_eq!(first.count, 2);
        assert_eq!(first.sum, 800.0);
        assert_eq!(first.max, 400.0);
    }

    #[test]
    fn the_aggregate_decides_which_number_a_day_is_worth() {
        let j = JournalId::new();
        let t = TrackerId::new();
        // Two headaches, a 3 and a 7. The day averaged 5 and was never a 10.
        let totals =
            ReadingQuery::default().totals(vec![reading(j, t, 1, 3.0), reading(j, t, 1, 7.0)]);
        let d = &totals[0];
        assert_eq!(d.value_for(Aggregate::Mean), 5.0);
        assert_eq!(d.value_for(Aggregate::Sum), 10.0);
        assert_eq!(d.value_for(Aggregate::Count), 2.0);
    }

    #[test]
    fn timed_only_excludes_the_readings_that_never_knew_their_minute() {
        let j = JournalId::new();
        let t = TrackerId::new();
        let at: Timestamp = "2026-03-01T08:00:00Z".parse().unwrap();
        let rows = vec![reading(j, t, 1, 1.0), Reading::at(j, t, at, "UTC", 1.0)];

        let all = ReadingQuery::default();
        assert_eq!(all.apply(rows.clone()).len(), 2);

        let timed = ReadingQuery { timed_only: true, ..Default::default() };
        let kept = timed.apply(rows);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].at.is_some());
    }

    #[test]
    fn an_aggregate_covers_the_window_however_short_the_list_is_capped_to() {
        // The two paths have to agree about which rows they cover, and a
        // limit is the one filter they cannot both honour: a partial sum is
        // not a smaller answer, it is a wrong one.
        let j = JournalId::new();
        let t = TrackerId::new();
        let rows = vec![reading(j, t, 1, 400.0), reading(j, t, 1, 400.0), reading(j, t, 1, 400.0)];

        let capped = ReadingQuery { limit: Some(1), ..Default::default() };
        assert_eq!(capped.apply(rows.clone()).len(), 1, "the list is capped");

        let totals = capped.totals(rows);
        assert_eq!(totals[0].count, 3, "the aggregate is not");
        assert_eq!(totals[0].sum, 1200.0);
    }

    #[test]
    fn a_day_of_unknown_times_reports_no_first_or_last() {
        let j = JournalId::new();
        let t = TrackerId::new();
        let totals = ReadingQuery::default().totals(vec![reading(j, t, 1, 1.0)]);
        assert_eq!(totals[0].first_at, None);
        assert_eq!(totals[0].last_at, None);
    }

    #[test]
    fn untimed_readings_sort_before_the_timed_ones_they_share_a_day_with() {
        let j = JournalId::new();
        let t = TrackerId::new();
        let morning: Timestamp = "2026-03-01T08:00:00Z".parse().unwrap();
        let rows = vec![Reading::at(j, t, morning, "UTC", 1.0), reading(j, t, 1, 2.0)];
        let sorted = ReadingQuery::default().apply(rows);
        assert_eq!(sorted[0].at, None);
        assert_eq!(sorted[1].at, Some(morning));
    }

    #[test]
    fn aad_is_bound_to_the_reading_it_seals() {
        let same = uuid::Uuid::now_v7();
        assert_ne!(reading_aad(ReadingId(same)), super::super::entry_aad(EntryId(same)));
    }
}

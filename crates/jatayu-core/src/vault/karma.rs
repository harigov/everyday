//! Karma, as the vault works it out: the deeds of each day, read from the
//! records every other app already keeps, and handed to the rules in
//! [`crate::karma`].
//!
//! One read per source over a span rather than one per day, so a quarter of
//! Karma costs five queries, not four hundred and fifty. A backend without a
//! domain contributes nothing from it rather than failing the report: a
//! vault with no library still finishes tasks.
//!
//! Two halves, kept apart on purpose. [`Vault::karma`] is a read and never
//! writes. [`Vault::settle_karma`] is the only writer: it starts the book
//! and settles the days old enough to be settled, and runs after an unlock.

use super::Vault;
use crate::error::{Error, Result};
use crate::id::ItemId;
use crate::karma::{
    self, Deed, DeedKind, GRACE_DAYS, KarmaBook, KarmaDay, KarmaReport, RULES_VERSION, SettledDay,
    habit_points,
};
use crate::model::{local_date_in, system_tz};
use crate::store::EntryQuery;
use crate::store::library::LogQuery;
use crate::store::purpose::PurposeWindow;
use crate::store::tasks::{TaskQuery, TaskSort};
use crate::store::trackers::{ReadingQuery, TrackerDay};
use crate::task::{Assignee, TaskStatus};
use jiff::civil::Date;
use std::collections::{BTreeMap, HashMap};

/// How many finished tasks are read at a time, newest first, while walking
/// back to the start of the span.
const TASK_PAGE: u32 = 200;

impl Vault {
    /// The book as it stands, or `None` before Karma has started.
    pub fn karma_book(&self) -> Result<Option<KarmaBook>> {
        self.read(|u| u.store.karma_book())
    }

    /// Start Karma if it has not started, and settle every day older than
    /// [`GRACE_DAYS`] that is not settled yet. Answers the book as written.
    ///
    /// The one place Karma is written, run after an unlock by a process that
    /// holds the vault's write claim. Idempotent: a second call the same day
    /// finds nothing to settle and writes nothing.
    pub fn settle_karma(&self, today: Date) -> Result<KarmaBook> {
        self.writable()?;
        let Some(mut book) = self.karma_book()? else {
            let book = KarmaBook::new(today);
            self.read(|u| u.store.put_karma_book(&book))?;
            return Ok(book);
        };
        let last = today.checked_sub(jiff::Span::new().days(GRACE_DAYS + 1)).unwrap_or(today);
        let first = book.first_open_day();
        if first > last {
            return Ok(book);
        }
        for (date, deeds) in self.deeds(first, last)? {
            let day = KarmaDay::score(date, deeds);
            if day.earned != 0 || day.lost != 0 {
                book.settled.push(SettledDay {
                    date,
                    earned: day.earned,
                    lost: day.lost,
                    net: day.net,
                });
            }
        }
        book.settled_through = Some(last);
        self.read(|u| u.store.put_karma_book(&book))?;
        Ok(book)
    }

    /// Karma since it started, with the days between `from` and `to` given
    /// one by one.
    ///
    /// The total is the settled days' sum plus the open days, worked out
    /// now. A settled day in the window is drawn as it was settled, with the
    /// deeds the records say today beside it; a settled day that came to
    /// nothing stays nothing. So a report scores the open days and the
    /// window, never the whole history: bounded however old Karma is.
    ///
    /// Before Karma has started -- the vault was never unlocked writable by
    /// this build, or the first unlock's settling has not run yet -- Karma
    /// is taken to start today, and nothing is written.
    pub fn karma(&self, from: Date, to: Date, today: Date) -> Result<KarmaReport> {
        let book = self.karma_book()?.unwrap_or_else(|| KarmaBook::new(today));
        let since = book.since;
        let open = book.first_open_day();
        let empty = KarmaReport {
            since,
            total: book.settled_total(),
            today: 0,
            days: Vec::new(),
            rules: RULES_VERSION,
        };
        if since > today {
            return Ok(empty);
        }
        let from = from.max(since);
        let to = to.min(today);
        let start = if from <= to { open.min(from) } else { open };
        let scored: BTreeMap<Date, KarmaDay> = if start <= today {
            self.deeds(start, today)?
                .into_iter()
                .map(|(date, deeds)| (date, KarmaDay::score(date, deeds)))
                .collect()
        } else {
            BTreeMap::new()
        };

        let open_total: i64 =
            scored.values().filter(|d| d.date >= open).map(|d| i64::from(d.net)).sum();
        let settled: HashMap<Date, SettledDay> =
            book.settled.iter().map(|d| (d.date, *d)).collect();
        let mut days = Vec::new();
        if from <= to {
            for (date, day) in scored.range(from..=to) {
                if *date >= open {
                    days.push(day.clone());
                } else if let Some(s) = settled.get(date) {
                    days.push(KarmaDay {
                        date: *date,
                        earned: s.earned,
                        lost: s.lost,
                        net: s.net,
                        deeds: day.deeds.clone(),
                    });
                }
            }
            // A settled day the records no longer show any deeds for still
            // came to what it came to.
            for s in book.settled.iter().filter(|s| s.date >= from && s.date <= to) {
                if !scored.contains_key(&s.date) {
                    days.push(KarmaDay {
                        date: s.date,
                        earned: s.earned,
                        lost: s.lost,
                        net: s.net,
                        deeds: Vec::new(),
                    });
                }
            }
            days.sort_by_key(|d| d.date);
        }
        Ok(KarmaReport {
            total: book.settled_total() + open_total,
            today: scored.get(&today).filter(|_| today >= open).map_or(0, |d| d.net),
            days,
            ..empty
        })
    }

    /// Every deed from `from` to `to`, by day.
    fn deeds(&self, from: Date, to: Date) -> Result<BTreeMap<Date, Vec<Deed>>> {
        let tz = system_tz();
        let mut out: BTreeMap<Date, Vec<Deed>> = BTreeMap::new();
        let mut add = |date: Date, deed: Deed| {
            if date >= from && date <= to && deed.points != 0 {
                out.entry(date).or_default().push(deed);
            }
        };

        // Tasks: the owner's, finished in the span. Walked newest first
        // until the finishes reach back past its start; there is no query by
        // finish date. Who a task is for is checked here rather than asked
        // of the store, because only a decrypted task says, and a filter on
        // it would make every page decrypt every finished task there is.
        if self.supports_tasks() {
            let projects: HashMap<_, _> =
                self.projects()?.into_iter().map(|p| (p.id, p.purpose.is_some())).collect();
            let mut offset = 0;
            'pages: loop {
                let page = self.tasks(&TaskQuery {
                    statuses: vec![TaskStatus::Done],
                    sort: TaskSort::CompletedDesc,
                    offset,
                    limit: Some(TASK_PAGE),
                    ..TaskQuery::default()
                })?;
                for task in &page {
                    let Some(at) = task.completed_at else { break 'pages };
                    let date = local_date_in(at, &tz);
                    if date < from {
                        break 'pages;
                    }
                    if task.assignee != Assignee::Owner {
                        continue;
                    }
                    let purposed = task.purpose.is_some()
                        || task.project_id.and_then(|p| projects.get(&p).copied()).unwrap_or(false);
                    add(
                        date,
                        Deed {
                            kind: DeedKind::Task,
                            label: task.title.clone(),
                            points: karma::task_points(purposed),
                        },
                    );
                }
                if page.len() < TASK_PAGE as usize {
                    break;
                }
                offset += TASK_PAGE;
            }
        }

        // Habits: only the trackers worth something either way -- archived
        // ones too, since a habit retired in March still kept (or broke) its
        // days before then. Asking for those alone keeps the derived
        // trackers that score nothing (time on a goal, things finished) from
        // being worked out for nothing.
        if self.supports_trackers() {
            let scoring: Vec<_> =
                self.trackers()?.into_iter().filter(|t| karma::points_for(t) != 0).collect();
            if !scoring.is_empty() {
                let query = ReadingQuery {
                    tracker_ids: scoring.iter().map(|t| t.id).collect(),
                    ..ReadingQuery::between(from, to)
                };
                let mut by_tracker: HashMap<_, Vec<TrackerDay>> = HashMap::new();
                for day in self.tracker_days_among(&query, Some(&scoring))? {
                    by_tracker.entry(day.tracker_id).or_default().push(day);
                }
                for tracker in &scoring {
                    for day in by_tracker.get(&tracker.id).into_iter().flatten() {
                        add(
                            day.date,
                            Deed {
                                kind: DeedKind::Habit,
                                label: tracker.name.clone(),
                                points: habit_points(tracker, Some(day)),
                            },
                        );
                    }
                }
            }
        }

        // The journal: written in on the day, by the owner -- the assistant's
        // own journal of notes to itself is not the owner writing.
        let assistant = self.assistant_journal()?.map(|j| j.id);
        let entries = self.entries(&EntryQuery {
            from: Some(from),
            to: Some(to),
            not_journal: assistant,
            ..EntryQuery::default()
        })?;
        let mut wrote: BTreeMap<Date, ()> = BTreeMap::new();
        for entry in entries {
            if karma::written_on_the_day(entry.local_date, local_date_in(entry.created_at, &tz)) {
                wrote.insert(entry.local_date, ());
            }
        }
        for date in wrote.into_keys() {
            add(
                date,
                Deed {
                    kind: DeedKind::Journal,
                    label: "Wrote in the journal".into(),
                    points: karma::JOURNAL_POINTS,
                },
            );
        }

        // Time recorded against a goal or an area.
        if self.supports_purpose() {
            let mut by_day: BTreeMap<Date, u64> = BTreeMap::new();
            for row in self.actual_minutes_by_day(PurposeWindow::new(from, to))? {
                if row.purpose.is_some() {
                    *by_day.entry(row.date).or_default() += row.minutes;
                }
            }
            for (date, minutes) in by_day {
                let hours = minutes / 60;
                add(
                    date,
                    Deed {
                        kind: DeedKind::Time,
                        label: format!(
                            "{hours} hour{} on your goals and areas",
                            if hours == 1 { "" } else { "s" }
                        ),
                        points: karma::time_points(minutes),
                    },
                );
            }
        }

        // Things got to the end of on a shelf, each item read once however
        // many times it was finished. An item the log names that is gone is
        // still a finish, without a title; any other failure is the answer.
        if self.supports_library() {
            let mut titles: HashMap<ItemId, String> = HashMap::new();
            for log in self.logs(&LogQuery::completions(from, to))? {
                let label = match titles.get(&log.item_id) {
                    Some(title) => title.clone(),
                    None => {
                        let title = match self.item(log.item_id) {
                            Ok(item) => item.title,
                            Err(Error::NotFound { .. }) => String::new(),
                            Err(e) => return Err(e),
                        };
                        titles.insert(log.item_id, title.clone());
                        title
                    }
                };
                add(
                    log.date,
                    Deed { kind: DeedKind::Finished, label, points: karma::FINISHED_POINTS },
                );
            }
        }

        Ok(out)
    }
}

//! Keeping in touch: the half that reads and writes the records.
//!
//! The arithmetic and the wording are [`crate::keepintouch`]'s, and when any of
//! this runs is `everyday_service::keepintouch`'s. Every write here can be
//! repeated: a row names its [source](LogSource), and the same source is not
//! written twice for the same person.

use super::Vault;
use crate::error::{Error, Result};
use crate::id::{EntryId, ItemId};
use crate::keepintouch::{
    KeepInTouch, KeepInTouchSettings, LOOK_BACK_DAYS, MAX_ASKS_PER_PASS, MAX_ENTRIES_PER_PASS,
    MAX_PEOPLE_AT_ONCE, Mention, NameMatch, PersonDue, TrackedTask, celebrates, cooling_off,
    every_days, fold, has_address, is_catch_up, match_guest, match_name, meeting_via,
    names_in_a_line, occasion_task, overdue_task, person_due, personal_dates, rank,
};
use crate::library::{CONTACT_SLUG, Item, ItemStatus, LogEntry, LogEvent, LogSource, Via};
use crate::model::EntrySummary;
use crate::proposal::{
    About, AboutKind, LogCandidate, PROPOSAL_DAYS, Payload, Proposal, ProposalKind, ProposalSource,
    ProposedLog, ProposedRecord,
};
use crate::store::EntryQuery;
use crate::store::calendars::EventQuery;
use crate::store::library::{ItemQuery, LogQuery};
use crate::store::proposals::ProposalQuery;
use crate::task::Task;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use std::collections::BTreeSet;

/// The hour of the morning a reminder for an occasion goes off.
const OCCASION_REMINDER_HOUR: i8 = 9;

/// Held across every read-modify-write of the [`KeepInTouch`] record.
///
/// The nightly pass, the minute's look for ticked-off tasks, a Settings save
/// and a deleted row can all be changing it at once, and without this each
/// would write back the copy it read a moment before, losing the others'
/// changes: a task no longer followed, a cool-off forgotten. Process-wide,
/// because only the process holding the vault's write claim writes at all.
static RECORD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What [`Vault::log_mentions`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MentionsLogged {
    pub logged: u32,
    pub asked: u32,
}

fn zone(tz: &str) -> TimeZone {
    TimeZone::get(tz).unwrap_or(TimeZone::UTC)
}

/// The spelling of a row's source and day that [`KeepInTouch::dismissed`]
/// keeps. The day is part of it because one thread or one recurring meeting is
/// a catch-up on every day it happens.
fn source_key(source: &LogSource, date: Date) -> String {
    match source {
        LogSource::Entry { id } => format!("entry:{id}@{date}"),
        LogSource::Thread { id } => format!("thread:{id}@{date}"),
        LogSource::Event { id } => format!("event:{id}@{date}"),
        LogSource::Task { id } => format!("task:{id}@{date}"),
    }
}

/// A catch-up about to be written, before anything has decided it should be.
struct CatchUp {
    person: ItemId,
    date: Date,
    via: Option<Via>,
    note: String,
    source: LogSource,
}

impl Vault {
    pub fn keep_in_touch(&self) -> Result<KeepInTouch> {
        self.with_library(|l| l.keep_in_touch())
    }

    /// Change the settings, leaving what the passes remember alone.
    pub fn save_keep_in_touch_settings(&self, settings: &KeepInTouchSettings) -> Result<()> {
        self.writable()?;
        settings.validate()?;
        self.update_keep_in_touch(|k| k.settings = settings.clone())
    }

    /// Read the record, change it, and write it back, with nothing else doing
    /// the same in between. See [`RECORD_LOCK`].
    pub fn update_keep_in_touch(&self, change: impl FnOnce(&mut KeepInTouch)) -> Result<()> {
        self.writable()?;
        let _held = RECORD_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.with_library(|l| {
            let mut record = l.keep_in_touch()?;
            change(&mut record);
            l.put_keep_in_touch(&record)
        })
    }

    /// The zone "today" is decided in: the assistant's setting, or the machine's.
    pub fn person_tz(&self) -> String {
        self.agent_settings().ok().and_then(|s| s.timezone).unwrap_or_else(crate::model::system_tz)
    }

    /// Everybody on the Contacts shelf. Empty when somebody has deleted it.
    pub fn contacts(&self) -> Result<Vec<Item>> {
        let kinds = self.kinds()?;
        let Some(shelf) = kinds.iter().find(|k| k.slug == CONTACT_SLUG) else {
            return Ok(Vec::new());
        };
        self.items(&ItemQuery::on_shelf(shelf.id))
    }

    /// Everybody with a frequency, most overdue first. Those with time left are
    /// included, with a negative [`PersonDue::days_over`].
    pub fn people_due(&self, today: Date, tz: &str) -> Result<Vec<PersonDue>> {
        let settings = self.keep_in_touch()?.settings;
        let zone = zone(tz);
        // Most of a shelf has no frequency at all.
        let wanted: Vec<Item> = self
            .contacts()?
            .into_iter()
            .filter(|item| every_days(item, &settings).is_some())
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        // One read of every catch-up in the vault rather than one per person:
        // this is asked on every redraw of the shelf and of Overview.
        let mut by_person: std::collections::HashMap<ItemId, Vec<LogEntry>> =
            std::collections::HashMap::new();
        let catch_ups =
            LogQuery { events: vec![LogEvent::Contact, LogEvent::Revisited], ..Default::default() };
        for log in self.logs(&catch_ups)? {
            by_person.entry(log.item_id).or_default().push(log);
        }
        let mut people = Vec::new();
        for item in wanted {
            let logs = by_person.remove(&item.id).unwrap_or_default();
            let added_on = item.created_at.to_zoned(zone.clone()).date();
            if let Some(due) = person_due(&item, &logs, &settings, today, added_on) {
                people.push(due);
            }
        }
        rank(&mut people);
        Ok(people)
    }

    // ---- writing catch-ups ----------------------------------------------

    /// Write a catch-up unless it is already there, or was deleted on purpose.
    ///
    /// "Already there" is the same source on the same day -- one long thread
    /// is a catch-up every day a message goes out in it -- or a catch-up that
    /// day by the same means, or one that day where either side does not say
    /// the means: the journal's account of a call somebody also logged by hand
    /// is one call.
    fn write_catch_up(&self, up: CatchUp, tz: &str, record: &KeepInTouch) -> Result<bool> {
        if record.was_dismissed(up.person, &source_key(&up.source, up.date)) {
            return Ok(false);
        }
        let logs = self.logs(&LogQuery::for_item(up.person))?;
        let there = logs.iter().any(|l| {
            l.date == up.date
                && (l.source == Some(up.source)
                    || (is_catch_up(l) && (l.via == up.via || l.via.is_none() || up.via.is_none())))
        });
        if there {
            return Ok(false);
        }
        let mut log = LogEntry::new(up.person, LogEvent::Contact, up.date, tz);
        log.via = up.via;
        log.note = up.note;
        log.source = Some(up.source);
        self.save_log(&log)?;
        self.mark_in_touch(up.person)?;
        Ok(true)
    }

    /// A card still saying "To meet" has been met.
    fn mark_in_touch(&self, person: ItemId) -> Result<()> {
        let mut item = self.item(person)?;
        if item.status == ItemStatus::Wishlist {
            item.status = ItemStatus::Active;
            item.updated_at = Timestamp::now();
            self.save_item(&item)?;
        }
        Ok(())
    }

    /// Say that a row something else wrote was wrong. See
    /// [`KeepInTouch::dismissed`].
    pub(super) fn dismiss_catch_up(&self, log: &LogEntry) -> Result<()> {
        let Some(source) = &log.source else { return Ok(()) };
        let key = source_key(source, log.date);
        self.update_keep_in_touch(|k| k.dismiss(log.item_id, key))
    }

    /// Stop asking about somebody the journal names. See
    /// [`KeepInTouch::ignored_names`].
    pub(super) fn ignore_name(&self, name: &str) -> Result<()> {
        let folded = fold(name);
        self.update_keep_in_touch(|k| {
            if !k.ignored_names.contains(&folded) {
                k.ignored_names.push(folded);
            }
        })
    }

    /// Accepting a [`ProposedLog`]: the card if there is one to make, then the
    /// row.
    pub(super) fn accept_catch_up(&self, proposed: &mut ProposedLog, now: Timestamp) -> Result<()> {
        if let Some(contact) = &mut proposed.new_contact {
            contact.updated_at = now;
            contact.status = ItemStatus::Active;
            self.save_item(contact)?;
            proposed.log.item_id = contact.id;
        }
        proposed.log.updated_at = now;
        self.save_log(&proposed.log)?;
        self.mark_in_touch(proposed.log.item_id)
    }

    // ---- the journal ----------------------------------------------------

    /// What a pass should read: this person's own writing, no older than
    /// [`LOOK_BACK_DAYS`], up to and including `through`, and new since the last
    /// nightly pass.
    ///
    /// The last clause by two tests rather than one. Anything written since that
    /// pass counts, and so does anything dated on or after the day it ran,
    /// whenever it was written: the pass that night left today's entry alone,
    /// and it may have been written before the pass ran.
    pub fn entries_to_read(
        &self,
        record: &KeepInTouch,
        today: Date,
        through: Date,
        tz: &str,
    ) -> Result<Vec<EntrySummary>> {
        let not_journal = self.assistant_journal().ok().flatten().map(|j| j.id);
        let from = today.checked_sub(Span::new().days(LOOK_BACK_DAYS)).unwrap_or(today);
        let summaries = self.entries(&EntryQuery {
            from: Some(from),
            to: Some(through),
            not_journal,
            ..Default::default()
        })?;
        let zone = zone(tz);
        let read_day = record.read_up_to.map(|at| at.to_zoned(zone.clone()).date());
        let mut wanted: Vec<EntrySummary> = summaries
            .into_iter()
            .filter(|s| {
                record.retry.contains(&s.id)
                    || match (record.read_up_to, read_day) {
                        (Some(at), Some(day)) => s.updated_at > at || s.local_date >= day,
                        _ => true,
                    }
            })
            .collect();
        wanted.sort_by_key(|s| (s.local_date, s.updated_at));
        // Too many: the newest are kept. The pass moves past all of them, and
        // what it lets go of should be the oldest, not this week's.
        if wanted.len() > MAX_ENTRIES_PER_PASS {
            tracing::warn!(entries = wanted.len(), "more to read than one pass reads");
            wanted.drain(..wanted.len() - MAX_ENTRIES_PER_PASS);
        }
        Ok(wanted)
    }

    /// Act on who an entry says you were in touch with: log the clear ones, and
    /// ask about the rest, at most `asks_left` of them.
    pub fn log_mentions(
        &self,
        entry: EntryId,
        date: Date,
        mentions: &[Mention],
        asks_left: u32,
        tz: &str,
        now: Timestamp,
    ) -> Result<MentionsLogged> {
        let people = self.contacts()?;
        let record = self.keep_in_touch()?;
        let mut done = MentionsLogged::default();
        let source = LogSource::Entry { id: entry };
        for mention in mentions {
            match match_name(&mention.name, &people) {
                NameMatch::One(person) => {
                    let up = CatchUp {
                        person,
                        date,
                        via: mention.via,
                        note: mention.note.clone(),
                        source,
                    };
                    if self.write_catch_up(up, tz, &record)? {
                        done.logged += 1;
                    }
                }
                found => {
                    if done.asked >= asks_left.min(MAX_ASKS_PER_PASS) {
                        continue;
                    }
                    let candidates = match found {
                        NameMatch::Several(ids) => ids,
                        _ => Vec::new(),
                    };
                    if self.ask_about(
                        mention,
                        entry,
                        date,
                        &people,
                        &candidates,
                        tz,
                        now,
                        &record,
                    )? {
                        done.asked += 1;
                    }
                }
            }
        }
        Ok(done)
    }

    /// Propose a catch-up that needs an answer first. `candidates` is who the
    /// name might mean, or empty if it means nobody on the shelf yet.
    #[allow(clippy::too_many_arguments)]
    fn ask_about(
        &self,
        mention: &Mention,
        entry: EntryId,
        date: Date,
        people: &[Item],
        candidates: &[ItemId],
        tz: &str,
        now: Timestamp,
        record: &KeepInTouch,
    ) -> Result<bool> {
        let settings = self.agent_settings().unwrap_or_default();
        if !settings.proposals.allows(ProposalKind::Log) {
            return Ok(false);
        }
        let folded = fold(&mention.name);
        if record.ignored_names.contains(&folded) {
            return Ok(false);
        }
        // One question about a name at a time, and the same entry's mention of
        // them only ever once, whatever became of the question.
        let source = LogSource::Entry { id: entry };
        let asked = self
            .proposals(&ProposalQuery { kinds: vec![ProposalKind::Log], ..Default::default() })?;
        let already = asked.iter().any(|p| {
            let Some(ProposedRecord::Log(l)) = p.payload.record() else { return false };
            fold(&l.asked_as) == folded && (p.is_pending() || l.log.source == Some(source))
        });
        if already {
            return Ok(false);
        }

        let names: Vec<LogCandidate> = candidates
            .iter()
            .filter_map(|id| {
                let item = people.iter().find(|p| p.id == *id)?;
                Some(LogCandidate { item_id: item.id, name: item.title.clone() })
            })
            .collect();
        let new_contact = if names.is_empty() {
            let kinds = self.kinds()?;
            let Some(shelf) = kinds.iter().find(|k| k.slug == CONTACT_SLUG) else {
                return Ok(false);
            };
            Some(Item::new(shelf.id, mention.name.trim()))
        } else {
            None
        };
        let person = new_contact.as_ref().map_or_else(|| names[0].item_id, |c| c.id);
        let mut log = LogEntry::new(person, LogEvent::Contact, date, tz);
        log.via = mention.via;
        log.note = mention.note.clone();
        log.source = Some(source);

        let day = date.strftime("%-d %B");
        let name = mention.name.trim();
        let caption = if new_contact.is_some() {
            format!("Add {name} to Contacts? You were in touch on {day}")
        } else {
            format!("Which {name} were you in touch with on {day}?")
        };
        let proposal = Proposal::new(
            Payload::Create {
                record: ProposedRecord::Log(Box::new(ProposedLog {
                    log,
                    new_contact,
                    candidates: names,
                    asked_as: name.to_string(),
                })),
            },
            caption,
            now,
            tz,
        )
        .with_why(mention.note.clone().chars().take(200).collect::<String>())
        .with_about(Some(About { kind: AboutKind::Entry, id: entry.to_string() }))
        .made_by(ProposalSource::KeepInTouch);
        // Forty already waiting is a limit reached, not a failure: the clear
        // names in this entry, and in the rest, are still logged.
        match self.save_proposal(&proposal) {
            Ok(()) => Ok(true),
            Err(Error::Invalid(reason)) => {
                tracing::debug!(%reason, "keeping in touch could not ask");
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    // ---- what needs no model -----------------------------------------------

    /// Log mail sent to people on the shelf since `from`. Sent to a handful at
    /// most: see [`MAX_PEOPLE_AT_ONCE`].
    pub fn log_sent_mail(&self, from: Timestamp, to: Timestamp, tz: &str) -> Result<u32> {
        if !self.supports_mail() || !self.supports_accounts() {
            return Ok(0);
        }
        let people: Vec<Item> = self
            .contacts()?
            .into_iter()
            .filter(|p| {
                p.facts.get(crate::keepintouch::FACT_EMAIL).is_some_and(|e| !e.trim().is_empty())
            })
            .collect();
        if people.is_empty() {
            return Ok(0);
        }
        let record = self.keep_in_touch()?;
        let zone = zone(tz);
        let mut logged = 0;
        for account in self.accounts()? {
            for message in self.mail_between(account.id, from, to)? {
                if !message.from.email.eq_ignore_ascii_case(account.address.trim()) {
                    continue;
                }
                let recipients: Vec<_> =
                    message.to.iter().chain(&message.cc).chain(&message.bcc).collect();
                if recipients.len() > MAX_PEOPLE_AT_ONCE {
                    continue;
                }
                let date = message.date.to_zoned(zone.clone()).date();
                for recipient in recipients {
                    let Some(person) = people.iter().find(|p| has_address(p, &recipient.email))
                    else {
                        continue;
                    };
                    // No subject: a row on the shelf is read by whoever may read
                    // the library, which is not everyone who may read the mail.
                    let up = CatchUp {
                        person: person.id,
                        date,
                        via: Some(Via::Email),
                        note: String::new(),
                        source: LogSource::Thread { id: message.thread_id },
                    };
                    if self.write_catch_up(up, tz, &record)? {
                        logged += 1;
                    }
                }
            }
        }
        Ok(logged)
    }

    /// Log meetings that have happened since `from` with people on the shelf,
    /// as [`meeting_via`] reads how. No title, for the reason sent mail has no
    /// subject.
    pub fn log_meetings(&self, from: Date, now: Timestamp, tz: &str) -> Result<u32> {
        if !self.supports_calendars() {
            return Ok(0);
        }
        let people = self.contacts()?;
        if people.is_empty() {
            return Ok(0);
        }
        let record = self.keep_in_touch()?;
        let today = now.to_zoned(zone(tz)).date();
        let mut logged = 0;
        for event in self.events(&EventQuery::between(from, today))? {
            if event.all_day
                || event.end > now
                || event.status == crate::calendar::EventStatus::Cancelled
                || event.attendees.len() > MAX_PEOPLE_AT_ONCE
            {
                continue;
            }
            let via = meeting_via(&event.url, &event.location, &event.description);
            let mut guests: BTreeSet<ItemId> = BTreeSet::new();
            for guest in event.attendees.iter().chain(std::iter::once(&event.organizer)) {
                if let Some(person) = match_guest(guest, &people) {
                    guests.insert(person);
                }
            }
            for person in guests {
                let up = CatchUp {
                    person,
                    date: event.local_date,
                    via,
                    note: String::new(),
                    source: LogSource::Event { id: event.id },
                };
                if self.write_catch_up(up, tz, &record)? {
                    logged += 1;
                }
            }
        }
        Ok(logged)
    }

    /// Count the tasks that were ticked off as the catch-ups they were for, and
    /// stop following the ones that have been settled either way. Returns how
    /// many rows it wrote.
    pub fn settle_touch_tasks(&self, now: Timestamp, tz: &str) -> Result<u32> {
        let record = self.keep_in_touch()?;
        if record.tracked.is_empty() {
            return Ok(0);
        }
        let zone = zone(tz);
        let today = now.to_zoned(zone.clone()).date();
        let mut logged = 0;
        let mut settled: Vec<crate::id::TaskId> = Vec::new();
        for tracked in &record.tracked {
            match self.task(tracked.task_id) {
                Ok(task) if task.status == crate::task::TaskStatus::Done => {
                    let done_on = task.completed_at.unwrap_or(now).to_zoned(zone.clone()).date();
                    for person in &tracked.people {
                        // Somebody may have deleted the card since.
                        if self.item(*person).is_err() {
                            continue;
                        }
                        let up = CatchUp {
                            person: *person,
                            date: done_on,
                            via: None,
                            note: String::new(),
                            source: LogSource::Task { id: task.id },
                        };
                        match self.write_catch_up(up, tz, &record) {
                            Ok(true) => logged += 1,
                            Ok(false) => {}
                            Err(e) => {
                                tracing::warn!(error = %e, "could not count a task as a catch-up")
                            }
                        }
                    }
                    settled.push(tracked.task_id);
                }
                Ok(task) if !task.status.is_open() => settled.push(tracked.task_id),
                Ok(_) => {}
                // Not there: still a proposal, or a proposal nobody accepted,
                // which is known for certain once that proposal would be gone.
                Err(Error::NotFound { .. }) => {
                    let waited =
                        today.since(tracked.made_on).map_or(0, |s| i64::from(s.get_days()));
                    if waited > PROPOSAL_DAYS {
                        settled.push(tracked.task_id);
                    }
                }
                // One task that cannot be read is left to be tried again; it
                // must not stop the others being counted.
                Err(e) => {
                    tracing::warn!(error = %e, "could not read a task keeping in touch follows")
                }
            }
        }
        if !settled.is_empty() {
            self.update_keep_in_touch(|k| k.tracked.retain(|t| !settled.contains(&t.task_id)))?;
        }
        Ok(logged)
    }

    // ---- what gets proposed --------------------------------------------------

    /// Propose getting in touch with whoever is furthest past due, up to what is
    /// left of this week's allowance. Returns how many.
    pub fn suggest_people(&self, today: Date, now: Timestamp, tz: &str) -> Result<u32> {
        if !self.agent_settings().unwrap_or_default().proposals.allows(ProposalKind::Task) {
            return Ok(0);
        }
        let record = self.keep_in_touch()?;
        let mut left = record.settings.per_week.saturating_sub(record.suggested_this_week(today));
        if left == 0 {
            return Ok(0);
        }
        let followed: BTreeSet<ItemId> =
            record.tracked.iter().flat_map(|t| t.people.iter().copied()).collect();
        let mut made = 0;
        for due in self.people_due(today, tz)? {
            if left == 0 {
                break;
            }
            if due.days_over < 0
                || followed.contains(&due.item_id)
                || record
                    .suggested
                    .get(&due.item_id)
                    .is_some_and(|day| cooling_off(*day, due.every_days, today))
            {
                continue;
            }
            let item = self.item(due.item_id)?;
            let (task, why) = overdue_task(&item, &due);
            let about = About { kind: AboutKind::Item, id: item.id.to_string() };
            if !self.propose_task(task, why, about, &[item.id], today, now, tz)? {
                break;
            }
            self.update_keep_in_touch(|k| {
                k.suggested.insert(due.item_id, today);
            })?;
            left -= 1;
            made += 1;
        }
        Ok(made)
    }

    /// Propose what the next few days' occasions call for: each person's own
    /// dates, and one task for each festival, for everybody who celebrates it.
    /// Returns how many.
    pub fn propose_occasions(&self, today: Date, now: Timestamp, tz: &str) -> Result<u32> {
        if !self.agent_settings().unwrap_or_default().proposals.allows(ProposalKind::Task) {
            return Ok(0);
        }
        let settings = self.keep_in_touch()?.settings;
        let lead = settings.lead_days;
        let horizon = today.checked_add(Span::new().days(i64::from(lead))).unwrap_or(today);
        // Rows outlive any lead allowed by `validate`, so these are enough to
        // know what has been proposed for a day, whatever became of it.
        let made = self.proposals(&ProposalQuery {
            kinds: vec![ProposalKind::Task],
            from: Some(today),
            to: Some(horizon),
            ..Default::default()
        })?;
        let people: Vec<Item> = self
            .contacts()?
            .into_iter()
            .filter(|p| !matches!(p.status, ItemStatus::Abandoned))
            .collect();
        let mut count = 0;

        for person in &people {
            for date in personal_dates(person, today, lead) {
                let id = person.id.to_string();
                let proposed = made.iter().any(|p| {
                    p.target_date == Some(date.on)
                        && p.caption == date.title
                        && p.about.as_ref().is_some_and(|a| a.id == id)
                });
                if proposed {
                    continue;
                }
                let task =
                    occasion_task(&date.title, &[person], date.on, reminder_for(date.on, now, tz));
                let about = About { kind: AboutKind::Item, id };
                if self.propose_task(
                    task,
                    format!("On {}", date.on.strftime("%A %-d %B")),
                    about,
                    &[person.id],
                    today,
                    now,
                    tz,
                )? {
                    count += 1;
                }
            }
        }

        if !self.supports_calendars() {
            return Ok(count);
        }
        let mut seen: Vec<(Date, String)> = Vec::new();
        for event in self.events(&EventQuery::between(today, horizon))? {
            if !event.all_day || event.local_date < today {
                continue;
            }
            // Two calendars carrying the same holiday are one holiday.
            let key = (event.local_date, fold(&event.title));
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let mut who: Vec<&Item> =
                people.iter().filter(|p| celebrates(p, &event.title)).collect();
            who.sort_by(|a, b| a.title.cmp(&b.title));
            if who.is_empty() {
                continue;
            }
            // Without the names, which is what finds this again tomorrow: the
            // title changes when somebody new starts celebrating it.
            let caption = format!("Send {} wishes", event.title.trim());
            let proposed = made
                .iter()
                .any(|p| p.target_date == Some(event.local_date) && p.caption == caption);
            if proposed {
                continue;
            }
            let names: Vec<String> = who.iter().map(|p| p.title.clone()).collect();
            let title = format!("{caption} to {}", names_in_a_line(&names));
            let ids: Vec<ItemId> = who.iter().map(|p| p.id).collect();
            let task = occasion_task(
                &title,
                &who,
                event.local_date,
                reminder_for(event.local_date, now, tz),
            );
            let about = About { kind: AboutKind::Event, id: event.id.to_string() };
            let why = format!("On {}", event.local_date.strftime("%A %-d %B"));
            if self.propose_named_task(task, caption, why, about, &ids, today, now, tz)? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Propose a task under its own title.
    #[allow(clippy::too_many_arguments)]
    fn propose_task(
        &self,
        task: Task,
        why: String,
        about: About,
        people: &[ItemId],
        today: Date,
        now: Timestamp,
        tz: &str,
    ) -> Result<bool> {
        let caption = task.title.clone();
        self.propose_named_task(task, caption, why, about, people, today, now, tz)
    }

    /// Propose a task and follow it. `false` when the proposals are full, which
    /// is a limit reached rather than anything gone wrong.
    #[allow(clippy::too_many_arguments)]
    fn propose_named_task(
        &self,
        task: Task,
        caption: String,
        why: String,
        about: About,
        people: &[ItemId],
        today: Date,
        now: Timestamp,
        tz: &str,
    ) -> Result<bool> {
        let task_id = task.id;
        let proposal =
            Proposal::new(Payload::Create { record: ProposedRecord::Task(task) }, caption, now, tz)
                .with_why(why)
                .with_about(Some(about))
                .made_by(ProposalSource::KeepInTouch);
        match self.save_proposal(&proposal) {
            Ok(()) => {}
            Err(Error::Invalid(reason)) => {
                tracing::debug!(%reason, "keeping in touch could not propose");
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        let tracked = TrackedTask { task_id, people: people.to_vec(), made_on: today };
        self.update_keep_in_touch(|k| k.tracked.push(tracked))?;
        Ok(true)
    }
}

/// That morning, if that morning has not already gone.
fn reminder_for(on: Date, now: Timestamp, tz: &str) -> Option<Timestamp> {
    let at = on.at(OCCASION_REMINDER_HOUR, 0, 0, 0).to_zoned(zone(tz)).ok()?.timestamp();
    (at > now).then_some(at)
}

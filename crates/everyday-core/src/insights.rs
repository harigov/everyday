//! Mail and the calendar, counted: what the Overview's mail, meeting and
//! people widgets draw.
//!
//! Pure functions over records somebody else has already read, so the rules
//! -- who counts as you, what counts as a meeting, which sender is a machine
//! -- are tested here without a store, and the service's two commands are a
//! read and a call.
//!
//! # Why this is not a query
//!
//! Every count here hangs on something sealed: who a message is from and who
//! it went to, what an event is called and who was invited. The clear
//! columns can say how many messages arrived on a day; they cannot say from
//! whom. So the window's records are decrypted and walked once, and the
//! window is what keeps that bounded -- the interface asks for at most a
//! quarter at a time.
//!
//! # What "you" means
//!
//! [`Me`]: every account's address and identity addresses, and every name
//! you send as or gave the profile. Mail is told apart by address, which is
//! exact. A calendar attendee is often only a name -- the feeds prefer one --
//! so meetings also match by name, compared without case.

use crate::account::Account;
use crate::calendar::{Event, EventStatus};
use crate::id::CalendarId;
use crate::mail::{Category, Message};
use crate::meeting::identify::parse_attendee;
use crate::profile::Profile;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// How many senders "who sends you the most" can list.
const TOP_SENDERS: usize = 30;

/// How many people the mail and meeting lists carry.
///
/// More than any card draws, because the interface joins the two lists into
/// one -- "who you are most in touch with" -- and somebody tenth in each is
/// often first in the sum.
const TOP_PEOPLE: usize = 100;

/// How many distinct event names "what takes the most time" can list.
const TOP_TITLES: usize = 30;

/// The most other people a meeting can have and still count as time with
/// each of them.
///
/// An all-hands is not time with anybody in particular. Counted, it puts
/// forty colleagues level with the manager you meet weekly, and the list
/// stops answering who you actually spend your time with.
pub const CROWD: usize = 8;

// ── who you are ─────────────────────────────────────────────────────────

/// The addresses and names that are you, compared without case.
#[derive(Debug, Clone, Default)]
pub struct Me {
    addresses: HashSet<String>,
    names: HashSet<String>,
}

impl Me {
    pub fn new<A, N>(addresses: A, names: N) -> Self
    where
        A: IntoIterator,
        A::Item: AsRef<str>,
        N: IntoIterator,
        N::Item: AsRef<str>,
    {
        let fold = |s: &str| s.trim().to_lowercase();
        Self {
            addresses: addresses
                .into_iter()
                .map(|a| fold(a.as_ref()))
                .filter(|a| !a.is_empty())
                .collect(),
            names: names.into_iter().map(|n| fold(n.as_ref())).filter(|n| !n.is_empty()).collect(),
        }
    }

    /// Every account's address and identities, the names those identities
    /// send as, and the profile's name. Not an account's `display_name`:
    /// that is what the account is called ("Personal Gmail"), not you.
    pub fn of(accounts: &[Account], profile: &Profile) -> Self {
        let addresses = accounts.iter().flat_map(|a| {
            std::iter::once(a.address.as_str())
                .chain(a.identities.iter().map(|i| i.address.as_str()))
        });
        let names = accounts
            .iter()
            .flat_map(|a| a.identities.iter().map(|i| i.name.clone()))
            .chain(std::iter::once(profile.name()));
        Self::new(addresses, names)
    }

    pub fn has_address(&self, email: &str) -> bool {
        self.addresses.contains(&email.trim().to_lowercase())
    }

    pub fn has_name(&self, name: &str) -> bool {
        self.names.contains(&name.trim().to_lowercase())
    }
}

// ── the window ──────────────────────────────────────────────────────────

/// The days `from..=to`, reckoned in `zone`.
#[derive(Debug, Clone)]
pub struct Window {
    pub from: Date,
    pub to: Date,
    pub zone: TimeZone,
}

impl Window {
    /// The first instant of `from`.
    pub fn start(&self) -> Timestamp {
        midnight(self.from, &self.zone)
    }

    /// The first instant *after* `to` -- an exclusive end, so a message sent
    /// at midnight lands on one day and not two.
    pub fn end(&self) -> Timestamp {
        midnight(self.to.tomorrow().unwrap_or(self.to), &self.zone)
    }

    fn date_of(&self, at: Timestamp) -> Date {
        at.to_zoned(self.zone.clone()).date()
    }

    fn days(&self) -> Vec<Date> {
        let mut out = Vec::new();
        let mut day = self.from;
        while day <= self.to {
            out.push(day);
            match day.tomorrow() {
                Ok(next) => day = next,
                Err(_) => break,
            }
        }
        out
    }
}

/// The first instant of `day` in `zone`. A day that begins inside a DST gap
/// begins when the clocks say it does.
fn midnight(day: Date, zone: &TimeZone) -> Timestamp {
    day.to_zoned(zone.clone()).map(|z| z.timestamp()).unwrap_or_else(|_| {
        // Out of jiff's range, which a date the interface sent never is.
        Timestamp::UNIX_EPOCH
    })
}

// ── mail ────────────────────────────────────────────────────────────────

/// A window of mail, counted.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailActivity {
    /// Messages that arrived from somebody else.
    pub received: u32,
    /// Messages you sent.
    pub sent: u32,
    /// Of `received`, how many are still unread.
    pub unread: u32,
    /// One row per day of the window, oldest first, the empty ones included.
    pub days: Vec<MailDay>,
    /// What `received` was, by category: each of the five, then the
    /// uncategorised, in that order, empty ones included.
    pub categories: Vec<CategoryCount>,
    /// Whoever sent the most, people and machines alike, most first.
    pub senders: Vec<Sender>,
    /// The people you exchanged mail with -- anyone you wrote to, and anyone
    /// who wrote to you who is not a newsletter or a notification -- most
    /// first by messages either way.
    pub correspondents: Vec<Correspondent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailDay {
    pub date: Date,
    pub received: u32,
    pub sent: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryCount {
    /// `None` is mail nothing has sorted yet.
    pub category: Option<Category>,
    pub messages: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sender {
    /// Lower-cased.
    pub email: String,
    /// The display name on their latest message, or empty.
    pub name: String,
    pub messages: u32,
    pub unread: u32,
    /// What most of their mail was sorted as.
    pub category: Option<Category>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Correspondent {
    /// Lower-cased.
    pub email: String,
    pub name: String,
    /// Messages from them.
    pub received: u32,
    /// Messages you sent with them in To, Cc or Bcc.
    pub sent: u32,
    /// The latest message either way.
    pub last: Timestamp,
}

/// Everything known about one address while the window is being walked.
#[derive(Default)]
struct Tally {
    name: String,
    named_at: Option<Timestamp>,
    received: u32,
    unread: u32,
    sent: u32,
    last: Option<Timestamp>,
    /// Received mail per category, indexed by [`category_slot`].
    kinds: [u32; 6],
}

impl Tally {
    fn saw(&mut self, name: &str, at: Timestamp) {
        if self.last.is_none_or(|last| at > last) {
            self.last = Some(at);
        }
        if !name.trim().is_empty() && self.named_at.is_none_or(|t| at >= t) {
            self.name = name.trim().to_string();
            self.named_at = Some(at);
        }
    }

    /// The category most of their received mail was sorted as. A tie goes to
    /// the earlier slot, so a sender split between important and newsletter
    /// reads as important.
    fn category(&self) -> Option<Category> {
        let mut best = 0;
        for slot in 1..self.kinds.len() {
            if self.kinds[slot] > self.kinds[best] {
                best = slot;
            }
        }
        CATEGORY_SLOTS[best]
    }

    /// A newsletter or a notification you never wrote back to.
    fn automated(&self) -> bool {
        self.sent == 0
            && matches!(self.category(), Some(Category::Newsletter | Category::Notification))
    }
}

const CATEGORY_SLOTS: [Option<Category>; 6] = [
    Some(Category::Priority),
    Some(Category::Important),
    Some(Category::Other),
    Some(Category::Newsletter),
    Some(Category::Notification),
    None,
];

fn category_slot(category: Option<Category>) -> usize {
    CATEGORY_SLOTS.iter().position(|c| *c == category).unwrap_or(CATEGORY_SLOTS.len() - 1)
}

/// Count a window of mail. `messages` may come from several accounts and in
/// any order; drafts are skipped, and anything dated outside the window too.
pub fn mail_activity(messages: &[Message], me: &Me, window: &Window) -> MailActivity {
    let (start, end) = (window.start(), window.end());
    let mut out = MailActivity::default();
    let mut days: BTreeMap<Date, (u32, u32)> =
        window.days().into_iter().map(|d| (d, (0, 0))).collect();
    let mut kinds = [0u32; 6];
    let mut people: HashMap<String, Tally> = HashMap::new();

    for message in messages {
        if message.flags.draft || message.date < start || message.date >= end {
            continue;
        }
        let day = days.entry(window.date_of(message.date)).or_default();

        if me.has_address(&message.from.email) {
            out.sent += 1;
            day.1 += 1;
            let mut named = HashSet::new();
            for to in message.to.iter().chain(&message.cc).chain(&message.bcc) {
                let email = to.email.trim().to_lowercase();
                if email.is_empty() || me.has_address(&email) || !named.insert(email.clone()) {
                    continue;
                }
                let tally = people.entry(email).or_default();
                tally.sent += 1;
                tally.saw(&to.name, message.date);
            }
            continue;
        }

        out.received += 1;
        day.0 += 1;
        let unread = message.flags.unread();
        if unread {
            out.unread += 1;
        }
        let slot = category_slot(message.category);
        kinds[slot] += 1;

        let email = message.from.email.trim().to_lowercase();
        if email.is_empty() {
            continue;
        }
        let tally = people.entry(email).or_default();
        tally.received += 1;
        tally.unread += u32::from(unread);
        tally.kinds[slot] += 1;
        tally.saw(&message.from.name, message.date);
    }

    out.days =
        days.into_iter().map(|(date, (received, sent))| MailDay { date, received, sent }).collect();
    out.categories = CATEGORY_SLOTS
        .iter()
        .zip(kinds)
        .map(|(category, messages)| CategoryCount { category: *category, messages })
        .collect();

    let mut senders: Vec<Sender> = people
        .iter()
        .filter(|(_, t)| t.received > 0)
        .map(|(email, t)| Sender {
            email: email.clone(),
            name: t.name.clone(),
            messages: t.received,
            unread: t.unread,
            category: t.category(),
        })
        .collect();
    senders.sort_by(|a, b| {
        b.messages.cmp(&a.messages).then(b.unread.cmp(&a.unread)).then(a.email.cmp(&b.email))
    });
    senders.truncate(TOP_SENDERS);
    out.senders = senders;

    let mut correspondents: Vec<Correspondent> = people
        .into_iter()
        .filter(|(_, t)| !t.automated())
        .map(|(email, t)| Correspondent {
            email,
            name: t.name,
            received: t.received,
            sent: t.sent,
            last: t.last.unwrap_or(start),
        })
        .collect();
    correspondents.sort_by(|a, b| {
        (b.received + b.sent)
            .cmp(&(a.received + a.sent))
            .then(b.sent.cmp(&a.sent))
            .then(a.email.cmp(&b.email))
    });
    correspondents.truncate(TOP_PEOPLE);
    out.correspondents = correspondents;
    out
}

// ── the calendar ────────────────────────────────────────────────────────

/// A window of the calendar, counted.
///
/// Only busy, timed events count: an all-day event is a label on a day, not
/// time spent; a cancelled one did not happen; and one marked free was put
/// there so it would not block anything. The same meeting on two calendars
/// -- yours and the team's -- is counted once.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingActivity {
    pub events: u32,
    /// Minutes those events cover, inside the window, with overlaps counted
    /// once -- two meetings at the same hour are one hour of your day.
    pub minutes: u32,
    /// One row per day of the window, oldest first, the empty ones included.
    pub days: Vec<MeetingDay>,
    /// Events grouped by name, most time first.
    pub titles: Vec<EventTitle>,
    /// The people in your meetings, most time first. Leaves you out, and
    /// leaves out meetings with more than [`CROWD`] other people.
    pub people: Vec<MeetingPerson>,
    /// How many meetings were left out of `people` for being that large.
    pub crowded: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDay {
    pub date: Date,
    /// Events starting on this day.
    pub events: u32,
    /// Minutes of this day under an event, overlaps counted once.
    pub minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTitle {
    /// As the latest of them spells it. Empty for events with no name.
    pub title: String,
    /// The calendar the latest of them is on, for its colour.
    pub calendar_id: CalendarId,
    pub events: u32,
    pub minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingPerson {
    /// A name where any of their invitations gave one, else their address.
    pub name: String,
    /// Lower-cased, where any invitation gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub meetings: u32,
    pub minutes: u32,
}

/// One event that counts, clipped to the window.
struct Slot<'a> {
    event: &'a Event,
    start: Timestamp,
    end: Timestamp,
}

impl Slot<'_> {
    fn minutes(&self) -> u32 {
        secs_to_minutes(self.end.duration_since(self.start).as_secs())
    }
}

fn secs_to_minutes(secs: i64) -> u32 {
    u32::try_from(secs.max(0) / 60).unwrap_or(u32::MAX)
}

/// Count a window of events. `events` may span several calendars and any
/// order; anything outside the window is ignored, and anything straddling
/// its edge counts only the part inside.
pub fn meeting_activity(events: &[Event], me: &Me, window: &Window) -> MeetingActivity {
    let (start, end) = (window.start(), window.end());

    let mut seen = HashSet::new();
    let mut slots: Vec<Slot> = events
        .iter()
        .filter(|e| !e.all_day && e.busy && e.status != EventStatus::Cancelled)
        .filter_map(|event| {
            let slot = Slot { event, start: event.start.max(start), end: event.end.min(end) };
            (slot.start < slot.end).then_some(slot)
        })
        .collect();
    slots.sort_by_key(|s| (s.event.start, s.event.end, s.event.id));
    slots.retain(|s| seen.insert((title_key(&s.event.title), s.event.start, s.event.end)));

    let mut out = MeetingActivity { events: slots.len() as u32, ..Default::default() };

    // ---- the days, overlaps merged -----------------------------------------
    let mut by_day: BTreeMap<Date, (u32, Vec<(Timestamp, Timestamp)>)> =
        window.days().into_iter().map(|d| (d, (0, Vec::new()))).collect();
    for slot in &slots {
        by_day.entry(window.date_of(slot.start)).or_default().0 += 1;
        // Split at each midnight it crosses, so an overnight event puts its
        // hours on the days they were spent.
        let mut from = slot.start;
        while from < slot.end {
            let day = window.date_of(from);
            let next = day.tomorrow().map(|d| midnight(d, &window.zone)).unwrap_or(slot.end);
            let until = if next > from { next.min(slot.end) } else { slot.end };
            by_day.entry(day).or_default().1.push((from, until));
            from = until;
        }
    }
    let mut total = 0i64;
    out.days = by_day
        .into_iter()
        .map(|(date, (events, mut spans))| {
            let secs = covered_secs(&mut spans);
            total += secs;
            MeetingDay { date, events, minutes: secs_to_minutes(secs) }
        })
        .collect();
    out.minutes = secs_to_minutes(total);

    // ---- by name ------------------------------------------------------------
    let mut titles: HashMap<String, EventTitle> = HashMap::new();
    for slot in &slots {
        let entry = titles.entry(title_key(&slot.event.title)).or_insert_with(|| EventTitle {
            title: String::new(),
            calendar_id: slot.event.calendar_id,
            events: 0,
            minutes: 0,
        });
        // `slots` is in start order, so the last write is the latest event.
        entry.title = slot.event.title.trim().to_string();
        entry.calendar_id = slot.event.calendar_id;
        entry.events += 1;
        entry.minutes += slot.minutes();
    }
    let mut titles: Vec<EventTitle> = titles.into_values().collect();
    titles.sort_by(|a, b| {
        b.minutes.cmp(&a.minutes).then(b.events.cmp(&a.events)).then(a.title.cmp(&b.title))
    });
    titles.truncate(TOP_TITLES);
    out.titles = titles;

    // ---- by person ----------------------------------------------------------
    //
    // A feed names an attendee by name where it has one and by address where
    // it does not, and the same person can arrive both ways. A name seen
    // beside exactly one address is folded into that address; a name seen
    // beside two is two people who share it, and stays apart.
    let invited: Vec<Vec<(Option<String>, Option<String>)>> = slots
        .iter()
        .map(|slot| {
            std::iter::once(&slot.event.organizer)
                .chain(&slot.event.attendees)
                .map(|raw| parse_attendee(raw))
                .filter(|(name, email)| name.is_some() || email.is_some())
                .filter(|(name, email)| {
                    !email.as_deref().is_some_and(|e| me.has_address(e))
                        && !name.as_deref().is_some_and(|n| me.has_name(n))
                })
                .collect()
        })
        .collect();

    let mut addresses_of: HashMap<String, HashSet<String>> = HashMap::new();
    for (name, email) in invited.iter().flatten() {
        if let (Some(name), Some(email)) = (name, email) {
            addresses_of.entry(name.to_lowercase()).or_default().insert(email.clone());
        }
    }
    let key_of = |name: &Option<String>, email: &Option<String>| -> String {
        if let Some(email) = email {
            return email.clone();
        }
        let name = name.as_deref().unwrap_or_default().to_lowercase();
        match addresses_of.get(&name) {
            Some(emails) if emails.len() == 1 => emails.iter().next().cloned().unwrap_or(name),
            _ => name,
        }
    };

    struct Person {
        name: Option<(Timestamp, String)>,
        email: Option<String>,
        meetings: u32,
        minutes: u32,
    }
    let mut people: HashMap<String, Person> = HashMap::new();
    for (slot, invited) in slots.iter().zip(&invited) {
        let mut here: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for (name, email) in invited {
            let entry = here.entry(key_of(name, email)).or_default();
            entry.0 = entry.0.take().or_else(|| name.clone());
            entry.1 = entry.1.take().or_else(|| email.clone());
        }
        if here.is_empty() {
            continue;
        }
        if here.len() > CROWD {
            out.crowded += 1;
            continue;
        }
        for (key, (name, email)) in here {
            let person = people.entry(key.clone()).or_insert_with(|| Person {
                name: None,
                email: key.contains('@').then(|| key.clone()),
                meetings: 0,
                minutes: 0,
            });
            if let Some(name) = name
                && person.name.as_ref().is_none_or(|(at, _)| slot.start >= *at)
            {
                person.name = Some((slot.start, name));
            }
            if person.email.is_none() {
                person.email = email;
            }
            person.meetings += 1;
            person.minutes += slot.minutes();
        }
    }
    let mut people: Vec<MeetingPerson> = people
        .into_iter()
        .map(|(key, p)| MeetingPerson {
            name: p.name.map(|(_, n)| n).or_else(|| p.email.clone()).unwrap_or(key),
            email: p.email,
            meetings: p.meetings,
            minutes: p.minutes,
        })
        .collect();
    people.sort_by(|a, b| {
        b.minutes.cmp(&a.minutes).then(b.meetings.cmp(&a.meetings)).then(a.name.cmp(&b.name))
    });
    people.truncate(TOP_PEOPLE);
    out.people = people;
    out
}

/// What makes two events "the same thing" by name: case and the spaces
/// around it are not a difference.
fn title_key(title: &str) -> String {
    title.trim().to_lowercase()
}

/// Seconds covered by the union of `spans`.
fn covered_secs(spans: &mut [(Timestamp, Timestamp)]) -> i64 {
    spans.sort();
    let mut total = 0;
    let mut current: Option<(Timestamp, Timestamp)> = None;
    for &(from, to) in spans.iter() {
        match current {
            Some((s, e)) if from <= e => current = Some((s, e.max(to))),
            Some((s, e)) => {
                total += e.duration_since(s).as_secs();
                current = Some((from, to));
            }
            None => current = Some((from, to)),
        }
    }
    if let Some((s, e)) = current {
        total += e.duration_since(s).as_secs();
    }
    total
}

/// The longest window either command will count, in days. A year of mail is
/// tens of thousands of messages to decrypt for a card on a dashboard.
pub const LONGEST_WINDOW: i64 = 366;

/// Is `from..=to` a window these commands will count?
pub fn window_is_reasonable(from: Date, to: Date) -> bool {
    from <= to && from.checked_add(LONGEST_WINDOW.days()).is_ok_and(|limit| to < limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{AccountId, EventId, MailMessageId, PackId, ThreadId};
    use crate::mail::{Address, CategorySource, MessageFlags};
    use crate::packstore::PackRef;
    use jiff::civil::date;

    fn window(from: Date, to: Date) -> Window {
        Window { from, to, zone: TimeZone::get("Europe/Lisbon").unwrap() }
    }

    fn at(day: Date, hour: i8, minute: i8) -> Timestamp {
        day.at(hour, minute, 0, 0)
            .to_zoned(TimeZone::get("Europe/Lisbon").unwrap())
            .unwrap()
            .timestamp()
    }

    fn me() -> Me {
        Me::new(["Me@Example.com", "alias@example.com"], ["Hari Govardhanam"])
    }

    fn mail(from: Address, to: &[Address], date: Timestamp) -> Message {
        let account = AccountId::new();
        Message {
            id: MailMessageId::new(),
            account_id: account,
            thread_id: ThreadId::new(),
            message_id_header: String::new(),
            date,
            from,
            to: to.to_vec(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: String::new(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
            invite: None,
        }
    }

    fn person(name: &str, email: &str) -> Address {
        Address { name: name.into(), email: email.into() }
    }

    #[test]
    fn mail_is_split_into_received_and_sent_by_your_own_addresses() {
        let day = date(2026, 10, 1);
        let ana = person("Ana", "ana@example.com");
        let mut read = mail(ana.clone(), &[person("", "me@example.com")], at(day, 9, 0));
        read.flags.seen = true;
        let unread = mail(ana.clone(), &[], at(day, 10, 0));
        let reply =
            mail(person("Me", "ALIAS@example.com"), std::slice::from_ref(&ana), at(day, 11, 0));
        let mut draft = mail(person("Me", "me@example.com"), &[ana], at(day, 12, 0));
        draft.flags.draft = true;

        let out = mail_activity(&[read, unread, reply, draft], &me(), &window(day, day));
        assert_eq!((out.received, out.sent, out.unread), (2, 1, 1), "a draft is neither");
        assert_eq!(out.days, vec![MailDay { date: day, received: 2, sent: 1 }]);
        assert_eq!(out.senders.len(), 1, "you are never your own sender");
        assert_eq!(out.senders[0].email, "ana@example.com");
        assert_eq!((out.senders[0].messages, out.senders[0].unread), (2, 1));
        assert_eq!(out.correspondents.len(), 1);
        assert_eq!((out.correspondents[0].received, out.correspondents[0].sent), (2, 1));
        assert_eq!(out.correspondents[0].last, at(day, 11, 0));
    }

    #[test]
    fn days_are_local_and_the_window_is_exclusive_at_its_end() {
        let (first, last) = (date(2026, 10, 1), date(2026, 10, 2));
        let ana = person("Ana", "ana@example.com");
        let messages = [
            // 23:30 in Lisbon is 22:30 UTC on the same day; a UTC bucket
            // would still say the 1st, so the test that matters is the next.
            mail(ana.clone(), &[], at(first, 23, 30)),
            // 00:30 local on the 2nd is 23:30 UTC on the 1st.
            mail(ana.clone(), &[], at(last, 0, 30)),
            mail(ana.clone(), &[], at(date(2026, 10, 3), 0, 0)),
            mail(ana, &[], at(first, 0, 0) - 1.second()),
        ];
        let out = mail_activity(&messages, &me(), &window(first, last));
        assert_eq!(out.received, 2, "midnight after the last day is outside it, as is before");
        assert_eq!(
            out.days,
            vec![
                MailDay { date: first, received: 1, sent: 0 },
                MailDay { date: last, received: 1, sent: 0 },
            ],
        );
    }

    #[test]
    fn senders_rank_by_volume_and_machines_are_not_correspondents() {
        let day = date(2026, 10, 1);
        let shop = person("Shop", "news@shop.example");
        let bob = person("Bob", "bob@example.com");
        let mut messages = Vec::new();
        for hour in 0..5 {
            let mut m = mail(shop.clone(), &[], at(day, hour, 0));
            m.category = Some(Category::Newsletter);
            messages.push(m);
        }
        let mut m = mail(bob.clone(), &[], at(day, 9, 0));
        m.category = Some(Category::Important);
        messages.push(m);

        let out = mail_activity(&messages, &me(), &window(day, day));
        let senders: Vec<_> = out.senders.iter().map(|s| (s.email.as_str(), s.messages)).collect();
        assert_eq!(senders, vec![("news@shop.example", 5), ("bob@example.com", 1)]);
        assert_eq!(out.senders[0].category, Some(Category::Newsletter));
        let people: Vec<_> = out.correspondents.iter().map(|c| c.email.as_str()).collect();
        assert_eq!(people, vec!["bob@example.com"], "a newsletter is not a person");

        let counts: Vec<_> = out.categories.iter().map(|c| (c.category, c.messages)).collect();
        assert_eq!(
            counts,
            vec![
                (Some(Category::Priority), 0),
                (Some(Category::Important), 1),
                (Some(Category::Other), 0),
                (Some(Category::Newsletter), 5),
                (Some(Category::Notification), 0),
                (None, 0),
            ],
        );
    }

    #[test]
    fn writing_back_to_a_notification_address_makes_it_a_correspondent() {
        let day = date(2026, 10, 1);
        let desk = person("Help desk", "help@vendor.example");
        let mut ticket = mail(desk.clone(), &[], at(day, 9, 0));
        ticket.category = Some(Category::Notification);
        let reply = mail(person("", "me@example.com"), &[desk], at(day, 10, 0));
        let out = mail_activity(&[ticket, reply], &me(), &window(day, day));
        assert_eq!(out.correspondents.len(), 1);
        assert_eq!(out.correspondents[0].name, "Help desk");
    }

    #[test]
    fn a_recipient_named_twice_on_one_message_counts_once() {
        let day = date(2026, 10, 1);
        let ana = person("Ana", "ana@example.com");
        let mut m = mail(person("", "me@example.com"), &[ana], at(day, 9, 0));
        m.cc = vec![person("", "ANA@example.com"), person("", "me@example.com")];
        let out = mail_activity(&[m], &me(), &window(day, day));
        assert_eq!(out.correspondents.len(), 1, "and you are not your own recipient");
        assert_eq!(out.correspondents[0].sent, 1);
        assert_eq!(out.correspondents[0].name, "Ana", "the name that came with the address");
    }

    fn event(title: &str, start: Timestamp, end: Timestamp, attendees: &[&str]) -> Event {
        let local = start.to_zoned(TimeZone::get("Europe/Lisbon").unwrap()).date();
        Event {
            id: EventId::new(),
            calendar_id: CalendarId::new(),
            uid: String::new(),
            title: title.into(),
            description: String::new(),
            location: String::new(),
            start,
            end,
            local_date: local,
            end_date: local,
            tz: "Europe/Lisbon".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: attendees.iter().map(|a| a.to_string()).collect(),
            url: String::new(),
            busy: true,
            series: None,
            updated_at: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn only_busy_timed_events_count_and_overlaps_count_once() {
        let day = date(2026, 10, 1);
        let mut all_day = event("Holiday", at(day, 0, 0), at(day, 23, 59), &[]);
        all_day.all_day = true;
        let mut free = event("Maybe", at(day, 8, 0), at(day, 9, 0), &[]);
        free.busy = false;
        let mut cancelled = event("Off", at(day, 8, 0), at(day, 9, 0), &[]);
        cancelled.status = EventStatus::Cancelled;
        let events = [
            all_day,
            free,
            cancelled,
            event("Standup", at(day, 9, 0), at(day, 10, 0), &[]),
            event("Review", at(day, 9, 30), at(day, 11, 0), &[]),
            // The same meeting on a second calendar.
            event(" standup ", at(day, 9, 0), at(day, 10, 0), &[]),
        ];
        let out = meeting_activity(&events, &me(), &window(day, day));
        assert_eq!(out.events, 2);
        assert_eq!(out.minutes, 120, "09:00 to 11:00, not 60 + 90");
        assert_eq!(out.days, vec![MeetingDay { date: day, events: 2, minutes: 120 }]);
        let titles: Vec<_> = out.titles.iter().map(|t| (t.title.as_str(), t.minutes)).collect();
        assert_eq!(titles, vec![("Review", 90), ("Standup", 60)]);
    }

    #[test]
    fn a_recurring_meeting_is_one_row_with_every_occurrence_summed() {
        let days = [date(2026, 10, 1), date(2026, 10, 2), date(2026, 10, 3)];
        let mut events: Vec<_> =
            days.iter().map(|d| event("1:1", at(*d, 9, 0), at(*d, 9, 30), &[])).collect();
        events.push(event("Planning", at(days[0], 14, 0), at(days[0], 15, 0), &[]));
        let out = meeting_activity(&events, &me(), &window(days[0], days[2]));
        let titles: Vec<_> =
            out.titles.iter().map(|t| (t.title.as_str(), t.events, t.minutes)).collect();
        assert_eq!(titles, vec![("1:1", 3, 90), ("Planning", 1, 60)]);
    }

    #[test]
    fn an_event_over_the_windows_edge_counts_only_the_inside() {
        let day = date(2026, 10, 2);
        let late = event("Overnight", at(date(2026, 10, 1), 23, 0), at(day, 1, 0), &[]);
        let out = meeting_activity(&[late], &me(), &window(day, day));
        assert_eq!(out.minutes, 60);
        assert_eq!(out.days, vec![MeetingDay { date: day, events: 1, minutes: 60 }]);
    }

    #[test]
    fn an_overnight_event_puts_its_hours_on_both_days() {
        let (first, second) = (date(2026, 10, 1), date(2026, 10, 2));
        let late = event("Launch", at(first, 23, 0), at(second, 1, 0), &[]);
        let out = meeting_activity(&[late], &me(), &window(first, second));
        assert_eq!(
            out.days,
            vec![
                MeetingDay { date: first, events: 1, minutes: 60 },
                MeetingDay { date: second, events: 0, minutes: 60 },
            ],
        );
    }

    #[test]
    fn people_leave_you_out_and_fold_a_name_into_its_address() {
        let day = date(2026, 10, 1);
        let events = [
            event(
                "1:1",
                at(day, 9, 0),
                at(day, 9, 30),
                &["Ana Lima <ana@example.com>", "me@example.com"],
            ),
            event("Lunch", at(day, 12, 0), at(day, 13, 0), &["Ana Lima", "hari govardhanam"]),
            event("Solo", at(day, 15, 0), at(day, 16, 0), &["Hari Govardhanam"]),
        ];
        let out = meeting_activity(&events, &me(), &window(day, day));
        assert_eq!(out.people.len(), 1, "a meeting with only you in it has nobody to count");
        let ana = &out.people[0];
        assert_eq!(ana.name, "Ana Lima");
        assert_eq!(ana.email.as_deref(), Some("ana@example.com"));
        assert_eq!((ana.meetings, ana.minutes), (2, 90));
    }

    #[test]
    fn the_organizer_counts_and_a_crowd_does_not() {
        let day = date(2026, 10, 1);
        let crowd: Vec<String> = (0..=CROWD).map(|n| format!("Person {n}")).collect();
        let crowd: Vec<&str> = crowd.iter().map(String::as_str).collect();
        let mut sync = event("Sync", at(day, 9, 0), at(day, 10, 0), &["Bo"]);
        sync.organizer = "Cy".into();
        let all_hands = event("All hands", at(day, 11, 0), at(day, 12, 0), &crowd);
        let out = meeting_activity(&[sync, all_hands], &me(), &window(day, day));
        let names: Vec<_> = out.people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Bo", "Cy"]);
        assert_eq!(out.crowded, 1);
        assert_eq!(out.minutes, 120, "a crowded meeting is still time on the calendar");
    }

    #[test]
    fn a_name_shared_by_two_addresses_is_not_guessed() {
        let day = date(2026, 10, 1);
        let events = [
            event("A", at(day, 9, 0), at(day, 10, 0), &["Sam <sam@one.example>"]),
            event("B", at(day, 10, 0), at(day, 11, 0), &["Sam <sam@two.example>"]),
            event("C", at(day, 11, 0), at(day, 12, 0), &["Sam"]),
        ];
        let out = meeting_activity(&events, &me(), &window(day, day));
        assert_eq!(out.people.len(), 3);
    }

    #[test]
    fn a_window_is_a_year_at_most_and_never_backwards() {
        assert!(window_is_reasonable(date(2026, 1, 1), date(2026, 1, 1)));
        assert!(window_is_reasonable(date(2026, 1, 1), date(2026, 12, 31)));
        assert!(!window_is_reasonable(date(2026, 1, 2), date(2026, 1, 1)));
        assert!(!window_is_reasonable(date(2025, 1, 1), date(2026, 1, 2)));
    }
}

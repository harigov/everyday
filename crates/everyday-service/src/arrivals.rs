//! Notifications for mail and calendar events that arrived on their own.
//!
//! The sync tasks already write the records and tell an open window to
//! reload. That is not the same as telling a person. A message they asked to
//! hear about that lands while the window is buried, and a meeting that
//! starts while they are in the journal, have to leave the process --
//! [`Reach::User`], which `ui/src/lib/notify.svelte.ts` routes to the
//! notification centre when the window is not in front and to a toast when
//! it is.
//!
//! What counts as news is decided here, with no vault and no clock of its
//! own, so the rules can be tested without either. The call sites are the
//! mail sync task, the model passes that file mail, the calendar sync
//! commands and the minute scheduler.
//!
//! # Mail, twice
//!
//! Which mail is wanted is
//! [`MailNotify`](everyday_core::account::MailNotify), and where a message is filed can
//! change after it arrives: the rules file it as it is stored, and a model
//! files it again a minute later. So both moments ask. A message wanted at
//! either is announced then, and
//! [`StatusRegistry::first_announcement`](crate::mailsync::status::StatusRegistry::first_announcement)
//! stops the second moment announcing it again. A model only speaks for what a
//! pass just stored -- see
//! [`StatusRegistry::note_arrival`](crate::mailsync::status::StatusRegistry::note_arrival)
//! -- since the rest of what it files was delivered long ago.

use std::collections::HashSet;

use everyday_core::calendar::{
    AccountCalendarSource, Calendar, CalendarOrigin, Event, EventStatus,
};
use everyday_core::id::{CalendarId, MailMessageId};
use everyday_core::mail::MailboxRole;
use everyday_core::store::calendars::EventQuery;
use everyday_core::{Result as CoreResult, Vault};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::events::{Level, Notification};
use crate::mailai::excerpt;
use crate::service::Service;

/// How old a message may be and still be news the first time this process
/// sees it. A first sync of an account is an archive; thirty-six hours is
/// "yesterday evening, still unread this morning" and not "every VIP from
/// 2019". An hour the other way is a sender's clock, not a reason to stay
/// quiet.
const NEWS_MAX_AGE_SECS: i64 = 36 * 60 * 60;
const NEWS_FUTURE_SKEW_SECS: i64 = 60 * 60;

/// How far ahead a newly synced event is still worth a notification. The
/// sync window itself runs two years forward, and a weekly meeting grows a
/// new occurrence at the far end of it on every refresh -- announcing that
/// would be a notification for a stand-up in 2028. Two weeks is a plan, not
/// an archive.
const FRESH_HORIZON_SECS: i64 = 14 * 24 * 60 * 60;
/// An event that ended more than this long ago is not news, even if this
/// sync is the first time the vault has stored it.
const FRESH_LOOKBACK_SECS: i64 = 60 * 60;
/// How far ahead the "before" snapshot reaches, in days. Wider than the
/// horizon, so an event moved from next month into next week is still
/// recognised as one the calendar already held; bounded at all because
/// every row read is a row unsealed, and the sync window runs two years.
const BEFORE_AHEAD_DAYS: i64 = 90;

/// How long after an event has started a reminder of it may still go out.
/// The scheduler runs once a minute, so this is a tick that landed late,
/// not a meeting that started while the app was quit an hour ago.
const REMIND_LOOKBACK_SECS: i64 = 2 * 60;

/// Past this many new events in one sync, one notification names the count
/// instead of a banner per event. The toast stack holds four; a calendar
/// that gained a dozen meetings should not fill it.
const FRESH_INDIVIDUAL_CAP: usize = 3;

/// A message to tell somebody about, as much of it as a notification needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMail {
    pub message_id: String,
    /// Already a display label -- a name where the header had one, the
    /// address otherwise. See [`sender_label`].
    pub from: String,
    pub subject: String,
}

/// What a headers pass knows about one message at the moment it is stored.
#[derive(Debug, Clone, Copy)]
pub struct IngestFacts {
    pub is_new: bool,
    pub unread: bool,
    pub in_inbox: bool,
    pub deleted: bool,
    pub draft: bool,
    /// The account's own address sent it.
    pub own: bool,
    /// The mailbox has finished a sync before this pass. False on the fill
    /// that brings an account's history in.
    pub mailbox_known: bool,
    pub sent_at: Timestamp,
    pub now: Timestamp,
}

/// A name if the header had one, otherwise the address, otherwise nothing.
pub fn sender_label(name: &str, email: &str) -> String {
    let name = name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    email.trim().to_string()
}

/// Does this message belong to the inbox, as opposed to Sent, All Mail with
/// no inbox label, or a folder the person did not ask to be told about?
///
/// Gmail stores the physical message in All Mail and says `\Inbox` with a
/// label. Everywhere else the inbox is a mailbox of its own.
pub fn in_inbox(role: MailboxRole, labels: &[String]) -> bool {
    match role {
        MailboxRole::Inbox => true,
        MailboxRole::All => labels.iter().any(|label| label == "\\Inbox"),
        _ => false,
    }
}

/// Did a message just stored by sync arrive, as far as anybody could want to
/// be told about it? Whether they do is
/// [`MailNotify`](everyday_core::account::MailNotify)'s to say.
///
/// Unread, somebody else's, and actually in the inbox. A mailbox this account
/// has already been syncing announces every such message -- a delayed one with
/// an old `Date` still just arrived. The first fill of a mailbox announces only
/// mail that is still current, so connecting an account does not announce the
/// archive and does announce this morning's.
pub fn ingest_is_news(facts: &IngestFacts) -> bool {
    if !facts.is_new
        || !facts.unread
        || !facts.in_inbox
        || facts.deleted
        || facts.draft
        || facts.own
    {
        return false;
    }
    facts.mailbox_known || is_recent(facts.sent_at, facts.now)
}

/// Is a message a model just filed where it is wanted still somewhere it is
/// news? It may have been read, archived or trashed since it arrived.
pub fn filed_is_news(facts: &FiledFacts) -> bool {
    facts.unread && facts.in_inbox && !facts.deleted && !facts.draft && !facts.own
}

/// What [`filed_is_news`] reads about the message a model just filed.
#[derive(Debug, Clone, Copy)]
pub struct FiledFacts {
    pub unread: bool,
    pub in_inbox: bool,
    pub deleted: bool,
    pub draft: bool,
    pub own: bool,
}

/// Has this account written in `message`'s thread? What
/// [`MailNotify::Replies`](everyday_core::account::MailNotify::Replies) asks of
/// a new message there, from the thread's own messages: somebody's own copy
/// of what they sent is in the thread on every provider, whichever client
/// they sent it from. A draft on the server is not something they said yet.
/// `own` is lower-case. By message and not by thread, because storing the
/// account's own answer can fold the thread this message started in into
/// another.
pub fn thread_has_own_message(
    vault: &Vault,
    message: MailMessageId,
    own: &HashSet<String>,
) -> bool {
    let Ok(message) = vault.mail_message(message) else { return false };
    vault.thread(message.thread_id).is_ok_and(|(_, messages)| {
        messages.iter().any(|m| !m.flags.draft && own.contains(&m.from.email.to_lowercase()))
    })
}

fn is_recent(sent_at: Timestamp, now: Timestamp) -> bool {
    let age = now.as_second() - sent_at.as_second();
    (-NEWS_FUTURE_SKEW_SECS..=NEWS_MAX_AGE_SECS).contains(&age)
}

/// One notification for a batch of new mail from a single pass.
///
/// `total` is how many messages the batch held, which can be more than
/// `mail` -- a pass keeps only the first few for their names. One message
/// names its sender. Several are one line, keyed on the account and the
/// batch's first message: a later batch, from the next pass or from a
/// model's own, posts its own line rather than replacing this one and losing
/// who wrote.
pub fn new_mail_notice(account_id: &str, mail: &[NewMail], total: usize) -> Option<Notification> {
    let total = total.max(mail.len());
    match mail {
        [] => None,
        [one] if total == 1 => Some(
            Notification::new(Level::Info, sender_or(one))
                .body(subject_or(one))
                .for_user()
                .key(format!("mail:{}", one.message_id)),
        ),
        many => {
            let preview = many
                .iter()
                .take(FRESH_INDIVIDUAL_CAP)
                .map(|item| format!("{}: {}", sender_or(item), subject_or(item)))
                .collect::<Vec<_>>()
                .join(" · ");
            Some(
                Notification::new(Level::Info, format!("{total} new emails"))
                    .body(preview)
                    .for_user()
                    .key(format!("mail-new:{account_id}:{}", many[0].message_id)),
            )
        }
    }
}

fn sender_or(mail: &NewMail) -> String {
    let from = mail.from.trim();
    if from.is_empty() { "New email".to_string() } else { excerpt(from, 80) }
}

fn subject_or(mail: &NewMail) -> String {
    let subject = mail.subject.trim();
    if subject.is_empty() { "(no subject)".to_string() } else { excerpt(subject, 140) }
}

/// An event, as much of it as "is this new, and is it soon?" needs.
#[derive(Debug, Clone)]
pub struct FreshEvent {
    pub uid: String,
    /// What stays the same when the event is moved -- see
    /// [`event_identity`]. What `before` holds.
    pub identity: String,
    pub title: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// The day an all-day event is on, in its own calendar's terms. `start`
    /// is midnight in that calendar's zone, which is not this machine's.
    pub local_date: Date,
    pub all_day: bool,
    pub cancelled: bool,
    /// The account's own event, made here, on a phone or anywhere else.
    /// Not news to the person who made it, and the one a write from this
    /// app brings back by its own resync.
    pub own: bool,
}

/// The part of an event's uid that survives it being moved.
///
/// A feed keys every occurrence `<UID>@<local start>` and CalDAV
/// `<href>#<start>`, so moving an event gives it a new uid; comparing
/// whole uids would announce a meeting moved from 10:00 to 11:00 as a new
/// one. The series' own UID, or the resource's href, is what was there
/// before. It is also shared by every occurrence of a repeating event, so
/// a series growing an occurrence is not news either -- it never was.
/// Google and Graph ids are stable across a move and are used whole.
pub fn event_identity<'a>(origin: &CalendarOrigin, uid: &'a str) -> &'a str {
    let split = match origin {
        CalendarOrigin::Url { .. } | CalendarOrigin::File { .. } => uid.rsplit_once('@'),
        CalendarOrigin::Account { source: AccountCalendarSource::CalDav, .. } => {
            uid.rsplit_once('#')
        }
        CalendarOrigin::Account { .. } => None,
    };
    split.map_or(uid, |(identity, _)| identity)
}

/// Events in `events` whose identity was not in `before`.
///
/// Cancelled events stay on the calendar struck through, and are not news.
/// Neither is the account's own event, nor anything that ended a while ago
/// or that starts past [`FRESH_HORIZON_SECS`]. The first sync of a calendar
/// has no honest `before`; the caller does not ask then.
pub fn select_fresh(
    before: &HashSet<String>,
    events: &[FreshEvent],
    now: Timestamp,
) -> Vec<FreshEvent> {
    let mut out: Vec<FreshEvent> = events
        .iter()
        .filter(|event| {
            !event.uid.is_empty()
                && !before.contains(&event.identity)
                && !event.cancelled
                && !event.own
                && event.end.as_second() >= now.as_second() - FRESH_LOOKBACK_SECS
                && event.start.as_second() <= now.as_second() + FRESH_HORIZON_SECS
        })
        .cloned()
        .collect();
    out.sort_by_key(|event| (event.start, event.uid.clone()));
    out
}

/// Notifications for [`select_fresh`]'s answer. A few events are named
/// individually; more than that is one line, or a calendar that gained a
/// conference's worth of sessions would post a conference's worth of banners.
pub fn fresh_notices(
    calendar_id: &str,
    calendar_name: &str,
    events: &[FreshEvent],
) -> Vec<Notification> {
    if events.is_empty() {
        return Vec::new();
    }
    if events.len() <= FRESH_INDIVIDUAL_CAP {
        return events.iter().map(|event| one_fresh(calendar_id, calendar_name, event)).collect();
    }
    let listed =
        events.iter().take(FRESH_INDIVIDUAL_CAP).map(event_title).collect::<Vec<_>>().join(", ");
    let name = calendar_label(calendar_name);
    vec![
        Notification::new(Level::Info, format!("{} new events on {name}", events.len()))
            .body(listed)
            .for_user()
            .key(format!("calendar-new:{calendar_id}")),
    ]
}

fn one_fresh(calendar_id: &str, calendar_name: &str, event: &FreshEvent) -> Notification {
    let when = when_label(event);
    let body = match calendar_name.trim() {
        "" => when,
        name => format!("{when} · {name}"),
    };
    Notification::new(Level::Info, event_title(event))
        .body(body)
        .for_user()
        .key(format!("event-new:{calendar_id}:{}", event.uid))
}

fn event_title(event: &FreshEvent) -> String {
    let title = event.title.trim();
    if title.is_empty() { "Calendar event".to_string() } else { excerpt(title, 80) }
}

fn calendar_label(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() { "your calendar".to_string() } else { name.to_string() }
}

fn when_label(event: &FreshEvent) -> String {
    if event.all_day {
        event.local_date.strftime("%a %-d %b, all day").to_string()
    } else {
        event.start.to_zoned(jiff::tz::TimeZone::system()).strftime("%a %-d %b, %H:%M").to_string()
    }
}

/// The identities `calendar` holds now, over the days a moved event could
/// have come from -- the "before" [`announce_fresh_events`] compares with.
/// Bounded, like that function's own read: see [`BEFORE_AHEAD_DAYS`].
pub fn event_identities(
    vault: &Vault,
    calendar: &Calendar,
    now: Timestamp,
) -> CoreResult<HashSet<String>> {
    let events = vault.events(&window_query(calendar.id, now, BEFORE_AHEAD_DAYS))?;
    Ok(events
        .iter()
        .map(|event| event_identity(&calendar.origin, &event.uid).to_string())
        .collect())
}

/// Tell the user about events this sync added. `before` is
/// [`event_identities`] from just before the sync; the caller does not call
/// this on a calendar's first sync, nor for a hidden calendar.
pub fn announce_fresh_events(
    svc: &Service,
    vault: &Vault,
    calendar: &Calendar,
    before: &HashSet<String>,
) {
    let now = svc.now();
    let Ok(events) = vault.events(&window_query(calendar.id, now, FRESH_HORIZON_SECS / 86_400 + 1))
    else {
        return;
    };
    let events = events.iter().map(|event| fresh_from(calendar, event)).collect::<Vec<_>>();
    let fresh = select_fresh(before, &events, now);
    for notice in fresh_notices(&calendar.id.to_string(), &calendar.name, &fresh) {
        svc.events().notify(notice);
    }
}

/// `id`'s events from yesterday to `ahead` days from now, in this machine's
/// days -- a day either side of the exact window, which the filters after
/// it settle.
fn window_query(id: CalendarId, now: Timestamp, ahead: i64) -> EventQuery {
    let today = now.to_zoned(jiff::tz::TimeZone::system()).date();
    EventQuery {
        calendar_id: Some(id),
        from: today.checked_sub(jiff::Span::new().days(1)).ok(),
        to: today.checked_add(jiff::Span::new().days(ahead)).ok(),
        ..Default::default()
    }
}

fn fresh_from(calendar: &Calendar, event: &Event) -> FreshEvent {
    FreshEvent {
        uid: event.uid.clone(),
        identity: event_identity(&calendar.origin, &event.uid).to_string(),
        title: event.title.clone(),
        start: event.start,
        end: event.end,
        local_date: event.local_date,
        all_day: event.all_day,
        cancelled: event.status == EventStatus::Cancelled,
        own: event.own == Some(true),
    }
}

/// An event the reminder tick is considering.
#[derive(Debug, Clone)]
pub struct ReminderEvent {
    pub calendar_id: CalendarId,
    pub uid: String,
    pub title: String,
    pub location: String,
    pub start: Timestamp,
    pub all_day: bool,
    pub cancelled: bool,
    /// Minutes before `start` to remind at, largest first -- what
    /// [`EventReminders::for_event`](everyday_core::EventReminders::for_event)
    /// made of its own alerts and the default.
    pub at: Vec<u32>,
}

/// A reminder whose moment has come: the event, and every one of its
/// alerts that has come round by now. Usually one; more after the machine
/// was asleep through an earlier one, in which case one notification says
/// how long is left and stands for them all.
#[derive(Debug, Clone)]
pub struct DueReminder {
    pub event: ReminderEvent,
    pub minutes: Vec<u32>,
}

/// The reminders due at `now`: every alert whose moment has passed, of an
/// event that has not been going on for longer than [`REMIND_LOOKBACK_SECS`].
///
/// An alert whose moment passed while the app was shut or the machine
/// asleep still goes out, as long as the event is still ahead: "starts in
/// five minutes" is worth having late, "started an hour ago" is not. Which
/// of them were already sent is the caller's to know.
pub fn due_reminders(events: &[ReminderEvent], now: Timestamp) -> Vec<DueReminder> {
    let now_s = now.as_second();
    let mut out: Vec<DueReminder> = events
        .iter()
        .filter(|event| {
            !event.cancelled
                && !event.uid.is_empty()
                && now_s <= event.start.as_second() + REMIND_LOOKBACK_SECS
        })
        .filter_map(|event| {
            let minutes: Vec<u32> = event
                .at
                .iter()
                .copied()
                .filter(|m| event.start.as_second() - i64::from(*m) * 60 <= now_s)
                .collect();
            (!minutes.is_empty()).then(|| DueReminder { event: event.clone(), minutes })
        })
        .collect();
    out.sort_by_key(|due| (due.event.start, due.event.uid.clone()));
    out
}

/// The notification for an event coming up: how long is left, in words that
/// suit how long that is, and where.
pub fn reminder_notice(event: &ReminderEvent, now: Timestamp, zone: &TimeZone) -> Notification {
    let mut body = when_it_starts(event, now, zone);
    let location = event.location.trim();
    if !location.is_empty() {
        body.push_str(" \u{b7} ");
        body.push_str(&excerpt(location, 60));
    }
    let title = event.title.trim();
    let title = if title.is_empty() { "Calendar event".to_string() } else { excerpt(title, 80) };
    Notification::new(Level::Info, title)
        .body(body)
        .for_user()
        .key(format!("event-soon:{}:{}", event.calendar_id, event.uid))
}

/// "Starting now", "Starts in 10 minutes", "Starts at 3:30 pm",
/// "Tomorrow at 9:00 am", "Fri 6 Nov at 9:00 am"; "Today" and "Tomorrow"
/// alone for an all-day event, which has no time to give.
fn when_it_starts(event: &ReminderEvent, now: Timestamp, zone: &TimeZone) -> String {
    let start = event.start.to_zoned(zone.clone());
    let today = now.to_zoned(zone.clone()).date();
    let day = match start.date() {
        d if d == today => None,
        d if today.tomorrow().ok() == Some(d) => Some("Tomorrow".to_string()),
        d => Some(d.strftime("%a %-d %b").to_string()),
    };
    if event.all_day {
        return day.unwrap_or_else(|| "Today".to_string());
    }
    let delta = event.start.as_second() - now.as_second();
    // To the nearest minute: a tick 4:59 out says five, not four.
    let minutes = (delta + 30) / 60;
    let clock = start.strftime("%-I:%M %P").to_string();
    match day {
        _ if delta <= 60 => "Starting now".to_string(),
        None if minutes == 1 => "Starts in 1 minute".to_string(),
        None if minutes < 60 => format!("Starts in {minutes} minutes"),
        None => format!("Starts at {clock}"),
        Some(day) => format!("{day} at {clock}"),
    }
}

/// How far ahead the reminder tick looks for events: as far as the longest
/// alert an event can carry, and a day more for the zones in between.
const REMIND_AHEAD_DAYS: i64 = (everyday_core::calendar::MAX_ALERT_MINUTES / (24 * 60)) as i64 + 1;

/// One pass of "something on the calendar is coming up".
///
/// Called from the minute scheduler, beside the meeting-notes watcher. A
/// meeting can receive both: that one asks about notes as it starts, this
/// one says it is coming, at the times its calendar or
/// [`EventReminders`](everyday_core::EventReminders) say.
pub fn tick_calendars(svc: &Service, vault: &Vault) {
    if !vault.supports_calendars() {
        return;
    }
    if let Err(e) = tick_calendars_inner(svc, vault) {
        tracing::warn!(error = %e, "calendar reminders: tick failed");
    }
}

fn tick_calendars_inner(svc: &Service, vault: &Vault) -> CoreResult<()> {
    let settings = vault.agent_settings().map(|s| s.reminders.events).unwrap_or_default();
    let now = svc.now();
    let zone = TimeZone::system();
    let today = now.to_zoned(zone.clone()).date();
    let events = vault.events(&EventQuery {
        from: today.checked_sub(jiff::Span::new().days(1)).ok(),
        to: today.checked_add(jiff::Span::new().days(REMIND_AHEAD_DAYS)).ok(),
        visible_only: true,
        ..Default::default()
    })?;
    let considered: Vec<ReminderEvent> = events
        .iter()
        .map(|event| ReminderEvent {
            calendar_id: event.calendar_id,
            uid: event.uid.clone(),
            title: event.title.clone(),
            location: event.location.clone(),
            start: event.start,
            all_day: event.all_day,
            cancelled: event.status == EventStatus::Cancelled,
            at: settings.for_event(event),
        })
        .filter(|event| !event.at.is_empty())
        .collect();
    for due in due_reminders(&considered, now) {
        // Last, as the meeting watcher does: marking an alert seen for a
        // reason that then skipped it would swallow the real reminder. Every
        // due alert is marked, so one asleep through two is said once.
        let mut fresh = false;
        for minutes in &due.minutes {
            let key = format!("{}#{minutes}", due.event.uid);
            fresh |= svc.calendar_reminder_seen(due.event.calendar_id, &key);
        }
        if fresh {
            svc.events().notify(reminder_notice(&due.event, now, &zone));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventSink, Reach};
    use everyday_core::calendar::AccountSyncCursor;
    use everyday_core::id::{AccountId, EventId};
    use std::sync::{Arc, Mutex};

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    fn facts(mailbox_known: bool) -> IngestFacts {
        let now = ts(1_700_000_000);
        IngestFacts {
            is_new: true,
            unread: true,
            in_inbox: true,
            deleted: false,
            draft: false,
            own: false,
            mailbox_known,
            sent_at: now,
            now,
        }
    }

    fn mail(id: &str, from: &str, subject: &str) -> NewMail {
        NewMail { message_id: id.into(), from: from.into(), subject: subject.into() }
    }

    #[test]
    fn a_new_unread_message_in_the_inbox_is_news() {
        assert!(ingest_is_news(&facts(true)));
    }

    #[test]
    fn anything_short_of_unread_inbox_mail_from_somebody_else_is_not_news() {
        let mut read = facts(true);
        read.unread = false;
        let mut own = facts(true);
        own.own = true;
        let mut seen_before = facts(true);
        seen_before.is_new = false;
        let mut archived = facts(true);
        archived.in_inbox = false;
        let mut deleted = facts(true);
        deleted.deleted = true;
        let mut draft = facts(true);
        draft.draft = true;
        for facts in [read, own, seen_before, archived, deleted, draft] {
            assert!(!ingest_is_news(&facts), "{facts:?}");
        }
    }

    #[test]
    fn a_first_sync_announces_recent_mail_and_not_the_archive() {
        let mut recent = facts(false);
        assert!(ingest_is_news(&recent), "this morning's mail is news even on the first fill");

        recent.sent_at = ts(recent.now.as_second() - NEWS_MAX_AGE_SECS - 1);
        assert!(!ingest_is_news(&recent), "a first fill must not announce the archive");

        let mut known_old = facts(true);
        known_old.sent_at = ts(known_old.now.as_second() - 400 * 24 * 60 * 60);
        assert!(
            ingest_is_news(&known_old),
            "a delayed message on a mailbox already syncing just arrived"
        );
    }

    #[test]
    fn gmail_all_mail_is_the_inbox_only_when_it_carries_the_label() {
        assert!(in_inbox(MailboxRole::Inbox, &[]));
        assert!(in_inbox(MailboxRole::All, &[String::from("\\Inbox")]));
        assert!(!in_inbox(MailboxRole::All, &[String::from("Travel")]));
        assert!(!in_inbox(MailboxRole::Sent, &[String::from("\\Inbox")]));
        assert!(!in_inbox(MailboxRole::Spam, &[]));
    }

    #[test]
    fn what_a_model_files_is_news_only_while_it_is_unread_in_the_inbox() {
        let news =
            FiledFacts { unread: true, in_inbox: true, deleted: false, draft: false, own: false };
        assert!(filed_is_news(&news));
        assert!(!filed_is_news(&FiledFacts { unread: false, ..news }), "already read");
        assert!(!filed_is_news(&FiledFacts { deleted: true, ..news }), "deleted");
        assert!(!filed_is_news(&FiledFacts { draft: true, ..news }), "a draft");
        assert!(!filed_is_news(&FiledFacts { own: true, ..news }), "the account's own");
        assert!(!filed_is_news(&FiledFacts { in_inbox: false, ..news }), "filtered past the inbox");
    }

    #[test]
    fn one_email_names_its_sender_and_several_are_one_line() {
        let one = new_mail_notice("acct", &[mail("m1", "Ada Lovelace", "The proof")], 1).unwrap();
        assert_eq!(one.title, "Ada Lovelace");
        assert_eq!(one.body.as_deref(), Some("The proof"));
        assert_eq!(one.key.as_deref(), Some("mail:m1"));
        assert!(matches!(one.reach, Reach::User));

        let many = new_mail_notice(
            "acct",
            &[mail("m1", "Ada", "One"), mail("m2", "Grace", "Two"), mail("m3", "Edsger", "Three")],
            3,
        )
        .unwrap();
        assert_eq!(many.title, "3 new emails");
        assert_eq!(many.key.as_deref(), Some("mail-new:acct:m1"), "one line per batch");
        assert!(many.body.unwrap().contains("Ada: One"));
        assert!(new_mail_notice("acct", &[], 0).is_none());
    }

    #[test]
    fn a_batch_counts_the_mail_it_did_not_keep() {
        let notice = new_mail_notice("acct", &[mail("m1", "Ada", "One")], 50).unwrap();
        assert_eq!(notice.title, "50 new emails");
    }

    #[test]
    fn a_blank_sender_still_reads_as_mail() {
        let notice = new_mail_notice("acct", &[mail("m1", "  ", "")], 1).unwrap();
        assert_eq!(notice.title, "New email");
        assert_eq!(notice.body.as_deref(), Some("(no subject)"));
    }

    fn fresh(uid: &str, start: i64, end: i64) -> FreshEvent {
        FreshEvent {
            uid: uid.into(),
            identity: uid.into(),
            title: format!("Event {uid}"),
            start: ts(start),
            end: ts(end),
            local_date: ts(start).to_zoned(jiff::tz::TimeZone::UTC).date(),
            all_day: false,
            cancelled: false,
            own: false,
        }
    }

    #[test]
    fn a_sync_announces_what_is_new_and_soon_and_not_the_users_own() {
        let now = ts(1_700_000_000);
        let soon = fresh("soon", now.as_second() + 3_600, now.as_second() + 7_200);
        let far = fresh(
            "far",
            now.as_second() + FRESH_HORIZON_SECS + 86_400,
            now.as_second() + FRESH_HORIZON_SECS + 90_000,
        );
        let old = fresh("old", now.as_second() - 86_400, now.as_second() - 80_000);
        let mut cancelled = fresh("off", now.as_second() + 3_600, now.as_second() + 7_200);
        cancelled.cancelled = true;
        let known = fresh("known", now.as_second() + 3_600, now.as_second() + 7_200);
        let mut mine = fresh("mine", now.as_second() + 3_600, now.as_second() + 7_200);
        mine.own = true;
        let events = vec![soon.clone(), far, old, cancelled, known.clone(), mine];

        let before = HashSet::from([known.uid.clone()]);
        let selected = select_fresh(&before, &events, now);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].uid, "soon");
    }

    #[test]
    fn a_moved_feed_or_caldav_event_keeps_its_identity() {
        let url = CalendarOrigin::Url { url: "https://example.com/a.ics".into() };
        let caldav = CalendarOrigin::Account {
            account_id: AccountId::new(),
            remote_id: "/cal/".into(),
            remote_name: "Work".into(),
            source: AccountCalendarSource::CalDav,
        };
        let google = CalendarOrigin::Account {
            account_id: AccountId::new(),
            remote_id: "primary".into(),
            remote_name: "Work".into(),
            source: AccountCalendarSource::Google,
        };
        assert_eq!(
            event_identity(&url, "abc@example.com@2026-10-23T10:00:00"),
            event_identity(&url, "abc@example.com@2026-10-23T11:00:00"),
        );
        assert_eq!(event_identity(&url, "abc@example.com@2026-10-23T10:00:00"), "abc@example.com");
        assert_eq!(
            event_identity(&caldav, "/cal/x.ics#2026-10-23 10:00:00 UTC"),
            event_identity(&caldav, "/cal/x.ics#2026-10-23 11:00:00 UTC"),
        );
        assert_eq!(event_identity(&google, "abc_20261023T100000Z"), "abc_20261023T100000Z");

        let now = ts(1_700_000_000);
        let mut moved = fresh("abc@11:00", now.as_second() + 3_600, now.as_second() + 7_200);
        moved.identity = "abc".into();
        assert!(select_fresh(&HashSet::from(["abc".to_string()]), &[moved], now).is_empty());
    }

    #[test]
    fn an_all_day_event_is_labelled_with_its_own_day() {
        // Midnight UTC on Friday is Thursday evening west of Greenwich;
        // the label must still say Friday.
        let mut holiday = fresh("day", 1_792_713_600, 1_792_800_000);
        holiday.all_day = true;
        holiday.local_date = jiff::civil::date(2026, 10, 23);
        assert_eq!(when_label(&holiday), "Fri 23 Oct, all day");
    }

    #[test]
    fn more_than_a_few_new_events_collapse_into_one_notification() {
        let now = ts(1_700_000_000);
        let events: Vec<_> = (0..4)
            .map(|i| fresh(&format!("e{i}"), now.as_second() + i, now.as_second() + 60))
            .collect();
        let notices = fresh_notices("cal", "Work", &events);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "4 new events on Work");
        assert_eq!(notices[0].key.as_deref(), Some("calendar-new:cal"));

        let one = fresh_notices("cal", "Work", &events[..1]);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].title, "Event e0");
        assert_eq!(one[0].key.as_deref(), Some("event-new:cal:e0"));
        assert!(one[0].body.as_deref().unwrap().contains("Work"));
    }

    fn reminding(uid: &str, start: Timestamp, at: &[u32]) -> ReminderEvent {
        ReminderEvent {
            calendar_id: CalendarId::new(),
            uid: uid.into(),
            title: "Design sync".into(),
            location: String::new(),
            start,
            all_day: false,
            cancelled: false,
            at: at.to_vec(),
        }
    }

    #[test]
    fn an_alert_is_due_from_its_moment_until_just_after_the_start() {
        let now = ts(1_700_000_000);
        let soon = reminding("soon", ts(now.as_second() + 5 * 60), &[10]);
        let later = reminding("later", ts(now.as_second() + 11 * 60), &[10]);
        let begun = reminding("begun", ts(now.as_second() - 3 * 60), &[10]);
        let mut cancelled = soon.clone();
        cancelled.uid = "off".into();
        cancelled.cancelled = true;

        let due = due_reminders(&[soon, later, begun, cancelled], now);
        assert_eq!(due.len(), 1, "{due:?}");
        assert_eq!(due[0].event.uid, "soon");
        assert_eq!(due[0].minutes, [10]);
    }

    /// Asleep through the day-before alert and woken inside the hour: both
    /// are due, and the caller says it once.
    #[test]
    fn every_alert_that_has_come_round_is_due_together() {
        let now = ts(1_700_000_000);
        let event = reminding("e", ts(now.as_second() + 30 * 60), &[24 * 60, 60, 15]);
        let due = due_reminders(&[event], now);
        assert_eq!(due[0].minutes, [24 * 60, 60]);
    }

    #[test]
    fn the_notice_says_how_long_is_left_in_words_that_fit() {
        let zone = TimeZone::UTC;
        // 2023-11-14 22:13:20 UTC.
        let now = ts(1_700_000_000);
        let say = |start: i64| {
            reminder_notice(&reminding("e", ts(now.as_second() + start), &[]), now, &zone)
                .body
                .unwrap()
        };
        assert_eq!(say(30), "Starting now");
        assert_eq!(say(75), "Starts in 1 minute");
        assert_eq!(say(4 * 60 + 59), "Starts in 5 minutes");
        assert_eq!(say(90 * 60), "Starts at 11:43 pm");
        assert_eq!(say(3 * 60 * 60), "Tomorrow at 1:13 am");
        assert_eq!(say(3 * 24 * 60 * 60), "Fri 17 Nov at 10:13 pm");

        let mut event = reminding("e", ts(now.as_second() + 5 * 60), &[]);
        event.location = "Room 4".into();
        let notice = reminder_notice(&event, now, &zone);
        assert_eq!(notice.title, "Design sync");
        assert_eq!(notice.body.as_deref(), Some("Starts in 5 minutes \u{b7} Room 4"));
        assert!(matches!(notice.reach, Reach::User));

        event.all_day = true;
        event.start = ts(now.as_second() + 2 * 60 * 60);
        assert_eq!(
            reminder_notice(&event, now, &zone).body.as_deref(),
            Some("Tomorrow \u{b7} Room 4")
        );
    }

    #[derive(Clone, Default)]
    struct TestSink {
        notifications: Arc<Mutex<Vec<Notification>>>,
    }

    impl EventSink for TestSink {
        fn notify(&self, notification: Notification) {
            self.notifications.lock().unwrap().push(notification);
        }
    }

    fn env() -> (Arc<Service>, Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            dir.path(),
            everyday_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: everyday_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault, dir)
    }

    fn account_calendar(vault: &Vault, name: &str) -> Calendar {
        let mut calendar = Calendar::subscribed(name, "https://example.com/work.ics");
        calendar.origin = CalendarOrigin::Account {
            account_id: AccountId::new(),
            remote_id: "primary".into(),
            remote_name: name.into(),
            source: AccountCalendarSource::Google,
        };
        vault.save_calendar(&calendar).unwrap();
        calendar
    }

    fn event_at(calendar_id: CalendarId, title: &str, offset_secs: i64, all_day: bool) -> Event {
        let now = Timestamp::now();
        let start = Timestamp::from_second(now.as_second() + offset_secs).unwrap();
        let end = Timestamp::from_second(start.as_second() + if all_day { 86_400 } else { 1_800 })
            .unwrap();
        let day = |at: Timestamp| at.to_zoned(jiff::tz::TimeZone::system()).date();
        Event {
            id: EventId::new(),
            calendar_id,
            uid: format!("{title}-{offset_secs}"),
            title: title.into(),
            description: String::new(),
            location: String::new(),
            start,
            end,
            local_date: day(start),
            end_date: day(end),
            tz: "UTC".into(),
            all_day,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: Vec::new(),
            url: String::new(),
            busy: true,
            series: None,
            own: None,
            alerts: Vec::new(),
            updated_at: now,
        }
    }

    fn seed(vault: &Vault, calendar_id: CalendarId, event: &Event) {
        vault
            .sync_account_calendar(
                calendar_id,
                std::slice::from_ref(event),
                &[],
                AccountSyncCursor::default(),
            )
            .unwrap();
    }

    #[test]
    fn a_timed_event_starting_soon_is_reminded_once() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let calendar = account_calendar(&vault, "Work");
        let soon = event_at(calendar.id, "Design sync", 5 * 60, false);
        let later = event_at(calendar.id, "Next week", 7 * 24 * 60 * 60, false);
        let all_day = event_at(calendar.id, "Holiday", 0, true);
        seed(&vault, calendar.id, &soon);
        seed(&vault, calendar.id, &later);
        seed(&vault, calendar.id, &all_day);

        tick_calendars(&svc, &vault);
        tick_calendars(&svc, &vault);

        let notices = sink.notifications.lock().unwrap();
        assert_eq!(
            notices.len(),
            1,
            "once, and not for the all-day event or the one a week away: {notices:?}"
        );
        assert_eq!(notices[0].title, "Design sync");
        assert!(matches!(notices[0].reach, Reach::User));
        assert_eq!(
            notices[0].key.as_deref(),
            Some(format!("event-soon:{}:{}", calendar.id, soon.uid).as_str())
        );
    }

    #[test]
    fn a_hidden_calendar_is_not_reminded() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let mut calendar = account_calendar(&vault, "Work");
        calendar.visible = false;
        vault.save_calendar(&calendar).unwrap();
        seed(&vault, calendar.id, &event_at(calendar.id, "Design sync", 60, false));

        tick_calendars(&svc, &vault);

        assert!(sink.notifications.lock().unwrap().is_empty());
    }

    fn remind_events(vault: &Vault, events: everyday_core::EventReminders) {
        let mut settings = vault.agent_settings().unwrap();
        settings.reminders.events = events;
        vault.save_agent_settings(&settings).unwrap();
    }

    /// The calendar said twenty minutes: that is when it is said, and the
    /// default's ten is not added to it.
    #[test]
    fn an_event_with_its_own_alert_is_reminded_then_and_not_at_the_default() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let calendar = account_calendar(&vault, "Work");
        let mut own = event_at(calendar.id, "Board", 15 * 60, false);
        own.alerts = vec![20];
        let plain = event_at(calendar.id, "Lunch", 15 * 60, false);
        seed(&vault, calendar.id, &own);
        seed(&vault, calendar.id, &plain);

        tick_calendars(&svc, &vault);

        let notices = sink.notifications.lock().unwrap();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].title, "Board");
        assert_eq!(notices[0].body.as_deref(), Some("Starts in 15 minutes"));
    }

    #[test]
    fn the_default_is_the_settings_and_own_alerts_can_be_ignored() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let calendar = account_calendar(&vault, "Work");
        let mut own = event_at(calendar.id, "Board", 25 * 60, false);
        own.alerts = vec![30];
        seed(&vault, calendar.id, &own);
        seed(&vault, calendar.id, &event_at(calendar.id, "Lunch", 25 * 60, false));
        remind_events(&vault, everyday_core::EventReminders { own: false, minutes: Some(30) });

        tick_calendars(&svc, &vault);
        assert_eq!(sink.notifications.lock().unwrap().len(), 2, "both, at the default's 30");

        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let calendar = account_calendar(&vault, "Work");
        let mut own = event_at(calendar.id, "Board", 5 * 60, false);
        own.alerts = vec![10];
        seed(&vault, calendar.id, &own);
        seed(&vault, calendar.id, &event_at(calendar.id, "Lunch", 5 * 60, false));
        remind_events(&vault, everyday_core::EventReminders { own: true, minutes: None });

        tick_calendars(&svc, &vault);
        let notices = sink.notifications.lock().unwrap();
        assert_eq!(notices.len(), 1, "no default: only the one with its own alert");
        assert_eq!(notices[0].title, "Board");
    }

    #[test]
    fn an_all_day_event_is_reminded_only_at_its_own_alert() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let calendar = account_calendar(&vault, "Work");
        let mut flight = event_at(calendar.id, "Flight", 60 * 60, true);
        flight.alerts = vec![15 * 60];
        seed(&vault, calendar.id, &flight);
        seed(&vault, calendar.id, &event_at(calendar.id, "Holiday", 60 * 60, true));

        tick_calendars(&svc, &vault);

        let notices = sink.notifications.lock().unwrap();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].title, "Flight");
    }

    #[test]
    fn a_new_event_on_a_calendar_already_synced_is_announced() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let calendar = account_calendar(&vault, "Work");
        let event = event_at(calendar.id, "Dentist", 2 * 24 * 60 * 60, false);
        seed(&vault, calendar.id, &event);

        announce_fresh_events(&svc, &vault, &calendar, &HashSet::new());
        let mut before = HashSet::new();
        before.insert(event.uid.clone());
        announce_fresh_events(&svc, &vault, &calendar, &before);

        let notices = sink.notifications.lock().unwrap();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].title, "Dentist");
        assert!(matches!(notices[0].reach, Reach::User));
        assert_eq!(
            notices[0].key.as_deref(),
            Some(format!("event-new:{}:{}", calendar.id, event.uid).as_str())
        );
    }
}

//! The watcher: "Take notes for Design sync?"
//!
//! Run every minute from [`crate::scheduler::tick`], while the vault is
//! unlocked. Each pass looks for calendar events that:
//!
//! * [`everyday_core::meeting::detect::is_online_call`] says are an online
//!   call, given the owner's own addresses (every mail account's address
//!   and its identities' addresses -- see [`owner_addresses`]);
//! * start between [`OFFER_LOOKBACK`] in the past and [`OFFER_LOOKAHEAD`]
//!   in the future, so an event is offered once, close to when it starts,
//!   rather than for the whole day it happens to fall on;
//! * pass the settings' [`CalendarFilter`](everyday_core::meeting::CalendarFilter)
//!   -- an explicit list of calendars, or of the roles their calendars
//!   serve, with an empty filter meaning every calendar;
//! * are not in `skipped_series` ("Never for this meeting");
//! * have not already been recorded (a recordings lookup by calendar and a
//!   window around the event's start, then a match on the event's own
//!   `uid`, since a feed event's `EventId` is not stable across a sync);
//! * and have not already been offered *this session* -- see
//!   [`crate::service::Service::meeting_offer_seen`].
//!
//! Every event that survives all of that gets a [`MeetingOffer`], with
//! `automatic` set from [`Offer::Always`] -- which is also, on the desktop
//! shell, what actually starts capturing; see `everyday-app`'s
//! `meeting::on_meeting_offer` -- and an ordinary [`Notification`] so the
//! offer reaches somebody who is not looking at the window.

use std::sync::Arc;

use everyday_core::calendar::{Calendar, Event};
use everyday_core::id::CalendarId;
use everyday_core::meeting::{CalendarFilter, Offer, detect};
use everyday_core::store::calendars::EventQuery;
use everyday_core::store::meetings::RecordingQuery;
use everyday_core::{Result as CoreResult, Vault};
use jiff::{Span, Timestamp, tz::TimeZone};

use crate::error::CommandResult;
use crate::events::{Level, MeetingOffer, Notification};
use crate::service::Service;

/// How long after an event has started it may still be offered.
const OFFER_LOOKBACK_SECS: i64 = 2 * 60;
/// How long before an event starts it may already be offered.
const OFFER_LOOKAHEAD_SECS: i64 = 60;

/// One pass. Failures are logged, not propagated -- a calendar the watcher
/// could not read this minute is not a reason to stop the scheduler's tick,
/// the same tolerance every other background pass it runs gets.
pub fn tick(svc: &Arc<Service>, vault: &Vault) {
    if let Err(e) = tick_inner(svc, vault) {
        tracing::warn!(error = %e, "meeting watcher: tick failed");
    }
}

fn tick_inner(svc: &Arc<Service>, vault: &Vault) -> CoreResult<()> {
    let settings = vault.meeting_settings()?;
    if !settings.enabled || settings.offer == Offer::Off {
        return Ok(());
    }

    let now = Timestamp::now();
    let owners = owner_addresses(vault)?;
    let calendars = vault.calendars()?;

    // A day either side of today, the same margin `scheduler::subjects_for`
    // gives its own `BeforeEvent` query: a call starting just after
    // midnight, looked at just before it, must not fall outside the query's
    // own window.
    let today = now.to_zoned(TimeZone::system()).date();
    let events = vault.events(&EventQuery {
        from: today.checked_sub(Span::new().days(1)).ok(),
        to: today.checked_add(Span::new().days(1)).ok(),
        ..Default::default()
    })?;

    for event in events {
        if !starts_in_window(&event, now) {
            continue;
        }
        if !detect::is_online_call(&event, &owners) {
            continue;
        }
        let Some(calendar) = calendars.iter().find(|c| c.id == event.calendar_id) else {
            continue;
        };
        if !calendar_allowed(&settings.calendars, calendar) {
            continue;
        }
        if settings.skipped_series.contains(&detect::series_key(&event.uid)) {
            continue;
        }
        if already_recorded(vault, &event)? {
            continue;
        }
        // Last, and only last: this both checks and marks "offered this
        // session" in one step, and every check above must run first, or a
        // tick that skipped an event for some *other* reason (already
        // recorded, say) would have marked it seen anyway and a later,
        // genuinely offerable pass over the same event would stay silent.
        if !svc.meeting_offer_seen(event.calendar_id, &event.uid) {
            continue;
        }

        let offer = MeetingOffer {
            event_id: event.id,
            calendar_id: event.calendar_id,
            uid: event.uid.clone(),
            title: event.title.clone(),
            start: event.start,
            end: event.end,
            calendar_name: calendar.name.clone(),
            automatic: settings.offer == Offer::Always,
        };
        svc.events().meeting_offer(offer);
        svc.events().notify(
            Notification::new(Level::Info, format!("{} is starting — take notes?", event.title))
                .for_user()
                .key(format!("meeting-offer:{}:{}", event.calendar_id, event.uid)),
        );
    }
    Ok(())
}

fn starts_in_window(event: &Event, now: Timestamp) -> bool {
    let starts = event.start.as_second();
    starts >= now.as_second() - OFFER_LOOKBACK_SECS
        && starts <= now.as_second() + OFFER_LOOKAHEAD_SECS
}

/// Does `calendar` pass `filter`? Empty lists (the default) mean every
/// calendar; otherwise a calendar passes by being named directly or by
/// serving one of the named roles.
fn calendar_allowed(filter: &CalendarFilter, calendar: &Calendar) -> bool {
    if filter.calendar_ids.is_empty() && filter.role_ids.is_empty() {
        return true;
    }
    filter.calendar_ids.contains(&calendar.id)
        || calendar.role_id.is_some_and(|role| filter.role_ids.contains(&role))
}

/// Every address this vault's owner is known by: every signed-in mail
/// account's own address, and every identity (alias) on it. There is
/// deliberately no read of `everyday_core::profile::Profile` here -- it
/// carries a name, not an address, so it has nothing [`detect::is_online_call`]
/// could compare an attendee against. A vault with no mail accounts answers
/// an empty list, which is not a bug: [`detect::is_online_call`]'s own rule
/// is that *any* attendee besides an owner address counts as somebody else,
/// so an empty owner list still finds a call, it merely cannot yet tell the
/// owner's own address apart from a guest's.
fn owner_addresses(vault: &Vault) -> CoreResult<Vec<String>> {
    if !vault.supports_accounts() {
        return Ok(Vec::new());
    }
    let mut addresses = Vec::new();
    for account in vault.accounts()? {
        addresses.push(account.address);
        addresses.extend(account.identities.into_iter().map(|i| i.address));
    }
    Ok(addresses)
}

/// Has a recording already been made of this event? Looked up by calendar
/// and a window around the event's start (a recording's own `started_at` is
/// approximately when the event started, not identical to it), then
/// confirmed by comparing the event's own `uid` -- the durable half, since
/// [`everyday_core::meeting::EventRef::uid`] is copied from it and a feed
/// calendar's `EventId` is not stable across a resync.
fn already_recorded(vault: &Vault, event: &Event) -> CoreResult<bool> {
    let margin = 6 * 60 * 60;
    let from = Timestamp::from_second(event.start.as_second() - margin).unwrap_or(event.start);
    let to = Timestamp::from_second(event.start.as_second() + margin).unwrap_or(event.start);
    let recordings = vault.recordings(&RecordingQuery {
        calendar_id: Some(event.calendar_id),
        from: Some(from),
        to: Some(to),
        ..Default::default()
    })?;
    Ok(recordings.iter().any(|r| r.event.as_ref().is_some_and(|e| e.uid == event.uid)))
}

/// `dismiss_meeting_offer`'s body: not offered again this session either
/// way, and -- for "Never for this meeting" -- not ever again, on any
/// session, for the whole series.
///
/// Takes `calendar_id` and `uid`, not an `EventId`: those are exactly what
/// [`svc.meeting_offer_seen`](Service::meeting_offer_seen) and
/// [`detect::series_key`] need, and both are durable across a feed resync
/// in a way an `EventId` is not (see [`MeetingOffer`](crate::events::MeetingOffer)'s
/// own doc). Deliberately does not look the event up at all: there is
/// nothing here an `Event` row would answer that these two fields do not
/// already carry, so a dismissal for an event a resync has since changed
/// -- or even removed -- still works exactly as well as one for an event
/// still sitting there unchanged.
pub fn dismiss(
    svc: &Arc<Service>,
    vault: &Vault,
    calendar_id: CalendarId,
    uid: &str,
    never: bool,
) -> CommandResult<()> {
    svc.meeting_offer_seen(calendar_id, uid);
    if never {
        let mut settings = vault.meeting_settings()?;
        settings.skipped_series.insert(detect::series_key(uid));
        vault.save_meeting_settings(&settings)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventSink;
    use everyday_core::calendar::{AccountCalendarSource, CalendarOrigin, EventStatus};
    use everyday_core::id::{AccountId, EventId, TemplateId};
    use everyday_core::meeting::{EventRef, MeetingSettings, Recording};
    use jiff::civil::date;
    use std::sync::Mutex as StdMutex;

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

    #[derive(Clone, Default)]
    struct TestSink {
        offers: Arc<StdMutex<Vec<MeetingOffer>>>,
        notifications: Arc<StdMutex<Vec<Notification>>>,
    }

    impl EventSink for TestSink {
        fn meeting_offer(&self, offer: MeetingOffer) {
            self.offers.lock().unwrap().push(offer);
        }
        fn notify(&self, notification: Notification) {
            self.notifications.lock().unwrap().push(notification);
        }
    }

    /// An account calendar, saved. Only an account calendar's origin is one
    /// [`Vault::sync_account_calendar`] accepts, which is the door this
    /// module's tests use to seed events without hand-writing iCalendar
    /// text.
    fn account_calendar(vault: &Vault) -> Calendar {
        let mut calendar = Calendar::subscribed("Work", "https://example.com/work.ics");
        calendar.origin = CalendarOrigin::Account {
            account_id: AccountId::new(),
            remote_id: "primary".into(),
            remote_name: "Work".into(),
            source: AccountCalendarSource::Google,
        };
        vault.save_calendar(&calendar).unwrap();
        calendar
    }

    /// An online call, starting `offset_secs` from now.
    fn online_event(calendar_id: CalendarId, offset_secs: i64) -> Event {
        let now = Timestamp::now();
        let start = Timestamp::from_second(now.as_second() + offset_secs).unwrap();
        let end = Timestamp::from_second(start.as_second() + 1_800).unwrap();
        Event {
            id: EventId::new(),
            calendar_id,
            uid: format!("evt-{}", EventId::new()),
            title: "Design sync".into(),
            description: "Join: https://meet.google.com/abc-defg-hij".into(),
            location: String::new(),
            start,
            end,
            local_date: date(2026, 9, 16),
            end_date: date(2026, 9, 16),
            tz: "UTC".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: "Alice <alice@example.com>".into(),
            attendees: vec!["Bob <bob@example.com>".into()],
            url: String::new(),
            busy: true,
            updated_at: now,
        }
    }

    fn seed_event(vault: &Vault, calendar_id: CalendarId, event: &Event) {
        vault
            .sync_account_calendar(
                calendar_id,
                std::slice::from_ref(event),
                &[],
                everyday_core::calendar::AccountSyncCursor::default(),
            )
            .unwrap();
    }

    fn enable(vault: &Vault, offer: Offer) {
        vault
            .save_meeting_settings(&MeetingSettings { enabled: true, offer, ..Default::default() })
            .unwrap();
    }

    // ---- selection --------------------------------------------------------

    #[test]
    fn an_online_call_starting_now_is_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        let offers = sink.offers.lock().unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].event_id, event.id);
        assert!(!offers[0].automatic);
        assert_eq!(sink.notifications.lock().unwrap().len(), 1, "a user-reach notification too");
    }

    #[test]
    fn always_mode_marks_the_offer_automatic() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Always);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap()[0].automatic);
    }

    #[test]
    fn off_mode_offers_nothing() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Off);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
    }

    #[test]
    fn an_event_with_no_join_link_is_not_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let mut event = online_event(calendar.id, 0);
        event.description.clear();
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
    }

    #[test]
    fn an_event_outside_the_window_is_not_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let far_future = online_event(calendar.id, OFFER_LOOKAHEAD_SECS + 3_600);
        seed_event(&vault, calendar.id, &far_future);
        let long_past = online_event(calendar.id, -(OFFER_LOOKBACK_SECS + 3_600));
        seed_event(&vault, calendar.id, &long_past);

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
    }

    #[test]
    fn a_calendar_left_out_of_the_filter_is_not_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let other_calendar = CalendarId::new();
        vault
            .save_meeting_settings(&MeetingSettings {
                enabled: true,
                offer: Offer::Ask,
                calendars: CalendarFilter { calendar_ids: vec![other_calendar], role_ids: vec![] },
                ..Default::default()
            })
            .unwrap();
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
    }

    #[test]
    fn a_series_marked_never_is_not_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        let mut settings =
            MeetingSettings { enabled: true, offer: Offer::Ask, ..Default::default() };
        settings.skipped_series.insert(detect::series_key(&event.uid));
        vault.save_meeting_settings(&settings).unwrap();
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
    }

    #[test]
    fn an_event_already_recorded_is_not_offered_again() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        let event_ref = EventRef {
            calendar_id: calendar.id,
            uid: event.uid.clone(),
            title: event.title.clone(),
            start: event.start,
            end: event.end,
            tz: "UTC".into(),
            organizer: String::new(),
            attendees: Vec::new(),
            join_url: String::new(),
            calendar_name: calendar.name.clone(),
        };
        let mut recording = Recording::new("Design sync", Some(event_ref), TemplateId::new());
        recording.started_at = event.start;
        vault.save_recording(&recording).unwrap();

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
    }

    #[test]
    fn the_same_event_is_offered_at_most_once_per_session() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);
        tick(&svc, &vault);

        assert_eq!(sink.offers.lock().unwrap().len(), 1);
    }

    // ---- dismiss ------------------------------------------------------

    #[test]
    fn dismissing_not_now_stops_it_being_offered_again_this_session() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        dismiss(&svc, &vault, calendar.id, &event.uid, false).unwrap();
        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
        assert!(
            !vault
                .meeting_settings()
                .unwrap()
                .skipped_series
                .contains(&detect::series_key(&event.uid)),
            "a plain dismissal must not touch the series"
        );
    }

    #[test]
    fn dismissing_never_adds_the_series_to_skipped_series() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        dismiss(&svc, &vault, calendar.id, &event.uid, true).unwrap();

        assert!(
            vault
                .meeting_settings()
                .unwrap()
                .skipped_series
                .contains(&detect::series_key(&event.uid))
        );
    }

    /// The bug this whole change exists to fix: the old `dismiss` looked
    /// the event up by its `EventId` before doing anything else, so a feed
    /// resync between the offer being raised and the click landing --
    /// which mints a *new* `EventId` for what is, by `uid`, the same
    /// occurrence -- made "Never for this meeting" silently fail on a
    /// `vault.event` lookup that could no longer find it. Deliberately
    /// never seeds an `Event` row here at all: `dismiss` now needs nothing
    /// but the calendar id and the uid the offer itself carried, so this
    /// must succeed exactly as if the event were still sitting there.
    #[test]
    fn dismissing_never_survives_the_event_having_been_resynced_away() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let uid = "evt-that-no-longer-exists@example.com";

        dismiss(&svc, &vault, calendar.id, uid, true).unwrap();

        assert!(
            vault.meeting_settings().unwrap().skipped_series.contains(&detect::series_key(uid))
        );
    }

    // ---- pure helpers ---------------------------------------------------

    #[test]
    fn calendar_allowed_lets_everything_through_an_empty_filter() {
        let (_, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        assert!(calendar_allowed(&CalendarFilter::default(), &calendar));
    }

    #[test]
    fn calendar_allowed_checks_the_named_ids() {
        let (_, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let filter = CalendarFilter { calendar_ids: vec![calendar.id], role_ids: vec![] };
        assert!(calendar_allowed(&filter, &calendar));
        let filter = CalendarFilter { calendar_ids: vec![CalendarId::new()], role_ids: vec![] };
        assert!(!calendar_allowed(&filter, &calendar));
    }
}

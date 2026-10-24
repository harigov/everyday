//! The watcher: "Take notes for Design sync?"
//!
//! Run every minute from [`crate::scheduler::tick`], while the vault is
//! unlocked. Each pass looks for calendar events that:
//!
//! * [`everyday_core::meeting::detect::is_meeting`] says are a meeting --
//!   timed, busy, not cancelled, with at least one attendee besides the
//!   owner, given the owner's own addresses (every mail account's address
//!   and its identities' addresses -- see [`owner_addresses`]). A join link
//!   is *not* required: an in-room meeting is recorded from the microphone
//!   alone, and the person asked for it to be offered all the same;
//! * start between [`OFFER_LOOKBACK_SECS`] in the past and
//!   [`OFFER_LOOKAHEAD_SECS`] in the future, so an event is offered once,
//!   close to when it starts, rather than for the whole day it happens to
//!   fall on;
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
//! Every event that survives all of that gets a [`MeetingOffer`], built by
//! `offer_for` -- the same builder [`now`] answers the shell's microphone
//! watcher with, so both kinds of offer have one shape. `automatic` is set
//! from [`Offer::Always`] *and* the setup being ready; on the desktop shell
//! that is also what actually starts capturing, see `everyday-app`'s
//! `offers::on_calendar_offer`.
//!
//! # Who is asked, and when
//!
//! See [`gate`]. [`Offer::Off`] never offers. Meeting notes that have never
//! been set up (no transcriber chosen) *do* offer, with `ready: false`, so
//! the interface can turn the offer into "Set up meeting notes" -- the
//! moment somebody is looking at a meeting is the moment they can see what
//! the feature is for. Meeting notes somebody set up and then turned off
//! offer nothing: that is an answer, and asking again would ignore it.
//!
//! # Reaching somebody who is not looking at the window
//!
//! This module raises the [`MeetingOffer`] event and nothing else. The
//! desktop shell turns each one into its own actionable desktop
//! notification, with buttons (`everyday-app`'s `offers.rs` and
//! `notice.rs`); a client with no shell -- a browser on the server build --
//! just shows the banner. An ordinary [`Notification`](crate::events::Notification)
//! sent alongside would have been a second, button-less notification for the
//! same offer.

use std::sync::Arc;

use everyday_core::calendar::{Calendar, Event};
use everyday_core::id::CalendarId;
use everyday_core::meeting::{CalendarFilter, MeetingSettings, Offer, detect};
use everyday_core::store::calendars::EventQuery;
use everyday_core::store::meetings::RecordingQuery;
use everyday_core::{Result as CoreResult, Vault};
use jiff::{Span, Timestamp, tz::TimeZone};

use crate::domains::meetings::MeetingNow;
use crate::error::CommandResult;
use crate::events::MeetingOffer;
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

/// What [`gate`] decided from the settings and the state of the setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gate {
    /// Offers are wanted at all.
    pub offers: bool,
    /// A recording could start right now.
    pub ready: bool,
}

/// Whether to offer, and whether an offer could be acted on.
///
/// `offers` is false for [`Offer::Off`], and for meeting notes somebody set
/// up (a transcriber is chosen) and then turned off; it is true when they
/// are on, *and* when they have never been set up, because that offer is how
/// the person finds out the feature exists. `ready` needs the switch on and
/// the transcriber usable -- [`crate::domains::meetings::view`], the same
/// check `begin` refuses a recording by, which reads keys and may look at
/// the model files on disk. That costs something, so the watcher's tick asks
/// [`wants_offers`] first and [`ready`] only once an event is actually
/// starting.
pub(crate) fn gate(vault: &Vault, settings: &MeetingSettings) -> CoreResult<Gate> {
    let offers = wants_offers(settings);
    Ok(Gate { offers, ready: offers && ready(vault, settings)? })
}

/// [`gate`]'s `offers`, which needs nothing but the settings.
fn wants_offers(settings: &MeetingSettings) -> bool {
    settings.offer != Offer::Off && (settings.enabled || settings.transcriber.is_none())
}

/// [`gate`]'s `ready`, the expensive half.
fn ready(vault: &Vault, settings: &MeetingSettings) -> CoreResult<bool> {
    Ok(settings.enabled && crate::domains::meetings::view(vault, settings.clone())?.usable)
}

/// The [`MeetingOffer`] for `event`, on `calendar`. One builder for the
/// watcher's own offers and for [`now`], so the shell never has two shapes
/// to handle.
///
/// `automatic` needs `ready` as well as [`Offer::Always`]: recording
/// "automatically" with no usable transcriber would only fail, and a
/// never-set-up vault must ask, not act.
fn offer_for(
    event: &Event,
    calendar: &Calendar,
    settings: &MeetingSettings,
    ready: bool,
) -> MeetingOffer {
    MeetingOffer {
        event_id: event.id,
        calendar_id: event.calendar_id,
        uid: event.uid.clone(),
        series: event.series.clone(),
        title: event.title.clone(),
        start: event.start,
        end: event.end,
        calendar_name: calendar.name.clone(),
        automatic: settings.offer == Offer::Always && ready,
        join_url: detect::join_link(event),
        ready,
    }
}

/// The events a day either side of today, the same margin
/// `scheduler::subjects_for` gives its own `BeforeEvent` query: a call
/// starting just after midnight, looked at just before it, must not fall
/// outside the query's own window.
fn nearby_events(vault: &Vault, now: Timestamp) -> CoreResult<Vec<Event>> {
    let today = now.to_zoned(TimeZone::system()).date();
    vault.events(&EventQuery {
        from: today.checked_sub(Span::new().days(1)).ok(),
        to: today.checked_add(Span::new().days(1)).ok(),
        ..Default::default()
    })
}

/// Everything about an event that decides whether it may be offered, other
/// than *when* it is and whether it was offered already: it is a meeting,
/// its calendar exists and passes the filter, its series is not skipped, and
/// it has not been recorded. The calendar it is on, if so.
fn eligible<'a>(
    vault: &Vault,
    settings: &MeetingSettings,
    owners: &[String],
    calendars: &'a [Calendar],
    event: &Event,
) -> CoreResult<Option<&'a Calendar>> {
    if !detect::is_meeting(event, owners) {
        return Ok(None);
    }
    let Some(calendar) = calendars.iter().find(|c| c.id == event.calendar_id) else {
        return Ok(None);
    };
    if !calendar_allowed(&settings.calendars, calendar) {
        return Ok(None);
    }
    if settings.skipped_series.contains(&detect::series_key_of(&event.uid, event.series.as_deref()))
    {
        return Ok(None);
    }
    if already_recorded(vault, event)? {
        return Ok(None);
    }
    Ok(Some(calendar))
}

fn tick_inner(svc: &Arc<Service>, vault: &Vault) -> CoreResult<()> {
    let settings = vault.meeting_settings()?;
    if !wants_offers(&settings) {
        return Ok(());
    }
    // Worked out at most once a tick, and only on a tick that has something
    // to offer -- see `gate`.
    let mut is_ready: Option<bool> = None;

    let now = svc.now();
    let owners = owner_addresses(vault)?;
    let calendars = vault.calendars()?;

    for event in nearby_events(vault, now)? {
        if !starts_in_window(&event, now) {
            continue;
        }
        let Some(calendar) = eligible(vault, &settings, &owners, &calendars, &event)? else {
            continue;
        };
        // Last, and only last: this both checks and marks "offered this
        // session" in one step, and every check above must run first, or a
        // tick that skipped an event for some *other* reason (already
        // recorded, say) would have marked it seen anyway and a later,
        // genuinely offerable pass over the same event would stay silent.
        if !svc.meeting_offer_seen(event.calendar_id, &event.uid) {
            continue;
        }

        let ready = match is_ready {
            Some(known) => known,
            None => *is_ready.insert(ready(vault, &settings)?),
        };
        svc.events().meeting_offer(offer_for(&event, calendar, &settings, ready));
    }
    Ok(())
}

/// How long before an event starts the microphone may already be taken for
/// it: somebody joins a call a few minutes early, and the shell asks the
/// moment the other application takes the microphone.
const NOW_LOOKAHEAD_SECS: i64 = 5 * 60;

/// `meeting_now`'s body: the shell's microphone watcher asking "what meeting
/// is on now, and may I offer?".
///
/// `event` is the meeting going on now -- starting at most five minutes from
/// now (so "joined early" still counts) and not yet over -- that passes
/// everything the watcher's own pass checks except the start window and
/// "offered this session"; if two overlap, the one that started most
/// recently, which is the one a person who has just picked up the
/// microphone is most likely in. `None` when offers are not wanted at all
/// ([`gate`]), or nothing qualifies -- the shell then offers a plain
/// recording with no event.
///
/// A read, deliberately: it never calls [`Service::meeting_offer_seen`].
/// The shell de-duplicates its own offers (one per time another application
/// takes the microphone), and marking the event seen here would silence the
/// watcher's own offer for it a minute later.
pub fn now(svc: &Arc<Service>, vault: &Vault) -> CoreResult<MeetingNow> {
    let settings = vault.meeting_settings()?;
    let Gate { offers, ready } = gate(vault, &settings)?;
    let mut answer = MeetingNow { offers, mic_offer: settings.mic_offer, ready, event: None };
    if !offers {
        return Ok(answer);
    }

    let now = svc.now();
    let owners = owner_addresses(vault)?;
    let calendars = vault.calendars()?;

    let mut events = nearby_events(vault, now)?;
    events.retain(|e| {
        e.start.as_second() <= now.as_second() + NOW_LOOKAHEAD_SECS
            && now.as_second() < e.end.as_second()
    });
    // Latest start first; the first one that passes is the answer.
    events.sort_by_key(|e| std::cmp::Reverse(e.start));
    for event in &events {
        if let Some(calendar) = eligible(vault, &settings, &owners, &calendars, event)? {
            answer.event = Some(offer_for(event, calendar, &settings, ready));
            break;
        }
    }
    Ok(answer)
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
/// carries a name, not an address, so it has nothing [`detect::is_meeting`]
/// could compare an attendee against. A vault with no mail accounts answers
/// an empty list, which is not a bug: [`detect::is_meeting`]'s own rule
/// is that *any* attendee besides an owner address counts as somebody else,
/// so an empty owner list still finds a meeting, it merely cannot yet tell the
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
/// Takes `calendar_id`, `uid` and `series`, not an `EventId`: those are
/// exactly what [`svc.meeting_offer_seen`](Service::meeting_offer_seen) and
/// [`detect::series_key_of`] need, and all are durable across a feed resync
/// in a way an `EventId` is not (see [`MeetingOffer`](crate::events::MeetingOffer)'s
/// own doc). `series` is the offer's own [`MeetingOffer::series`] --
/// whatever the adapter that produced the event put on [`Event::series`],
/// carried through untouched -- so the key this computes is exactly the one
/// [`tick_inner`] checked the event against, including for a recurring
/// Google or Graph event whose `uid` is only unique per occurrence.
/// Deliberately does not look the event up at all: there is nothing here an
/// `Event` row would answer that these fields do not already carry, so a
/// dismissal for an event a resync has since changed -- or even removed --
/// still works exactly as well as one for an event still sitting there
/// unchanged.
pub fn dismiss(
    svc: &Arc<Service>,
    vault: &Vault,
    calendar_id: CalendarId,
    uid: &str,
    series: Option<&str>,
    never: bool,
) -> CommandResult<()> {
    svc.meeting_offer_seen(calendar_id, uid);
    if never {
        let mut settings = vault.meeting_settings()?;
        settings.skipped_series.insert(detect::series_key_of(uid, series));
        vault.save_meeting_settings(&settings)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventSink, Notification};
    use everyday_core::calendar::{AccountCalendarSource, CalendarOrigin, EventStatus};
    use everyday_core::id::{AccountId, EventId, TemplateId};
    use everyday_core::meeting::{EventRef, MeetingSettings, Recording, TranscriberConfig};
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
        // The day it falls on where the test runs, as a real sync would
        // file it. `tick` asks for events a day either side of today by
        // this date, so a fixed one quietly stopped matching a day after
        // it was written.
        let day = |at: Timestamp| at.to_zoned(TimeZone::system()).date();
        Event {
            id: EventId::new(),
            calendar_id,
            uid: format!("evt-{}", EventId::new()),
            title: "Design sync".into(),
            description: "Join: https://meet.google.com/abc-defg-hij".into(),
            location: String::new(),
            start,
            end,
            local_date: day(start),
            end_date: day(end),
            tz: "UTC".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: "Alice <alice@example.com>".into(),
            attendees: vec!["Bob <bob@example.com>".into()],
            url: String::new(),
            busy: true,
            series: None,
            own: None,
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

    /// Meeting notes switched on, with a transcriber that is usable on any
    /// machine: a loopback "compatible" server needs no key and no speech kit
    /// (see `domains::meetings::usable`), so `ready` is true wherever this
    /// runs.
    fn set_up(offer: Offer) -> MeetingSettings {
        MeetingSettings {
            enabled: true,
            offer,
            transcriber: Some(TranscriberConfig::Compatible {
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "whisper-1".into(),
            }),
            ..Default::default()
        }
    }

    fn enable(vault: &Vault, offer: Offer) {
        vault.save_meeting_settings(&set_up(offer)).unwrap();
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
        assert!(offers[0].ready);
        assert!(
            sink.notifications.lock().unwrap().is_empty(),
            "the desktop shell raises its own notification; the service sends none"
        );
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

        let offers = sink.offers.lock().unwrap();
        assert!(offers[0].automatic);
        assert!(offers[0].ready);
    }

    /// Never set up is not the same as turned off: the offer is made, as a
    /// prompt to set meeting notes up, and `Always` must not start anything.
    #[test]
    fn a_vault_never_set_up_still_offers_but_not_ready_and_never_automatic() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        vault
            .save_meeting_settings(&MeetingSettings { offer: Offer::Always, ..Default::default() })
            .unwrap();
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        let offers = sink.offers.lock().unwrap();
        assert_eq!(offers.len(), 1);
        assert!(!offers[0].ready);
        assert!(!offers[0].automatic);
    }

    #[test]
    fn meeting_notes_set_up_and_then_turned_off_offer_nothing() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        vault
            .save_meeting_settings(&MeetingSettings { enabled: false, ..set_up(Offer::Ask) })
            .unwrap();
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        assert!(sink.offers.lock().unwrap().is_empty());
    }

    #[test]
    fn the_offer_carries_the_join_link_from_the_description() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        let offers = sink.offers.lock().unwrap();
        assert_eq!(offers[0].join_url.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
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
    fn an_in_room_meeting_with_no_join_link_is_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let mut event = online_event(calendar.id, 0);
        event.description.clear();
        seed_event(&vault, calendar.id, &event);

        tick(&svc, &vault);

        let offers = sink.offers.lock().unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].join_url, None);
    }

    #[test]
    fn an_event_with_nobody_else_invited_is_not_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let mut event = online_event(calendar.id, 0);
        event.attendees.clear();
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
                calendars: CalendarFilter { calendar_ids: vec![other_calendar], role_ids: vec![] },
                ..set_up(Offer::Ask)
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
        let mut settings = set_up(Offer::Ask);
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
            series: event.series.clone(),
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

    // ---- now ----------------------------------------------------------

    /// A meeting that began `started_ago_secs` ago and runs for half an hour
    /// from then, so `0` is "just started" and `1_200` is still going.
    fn running_event(calendar_id: CalendarId, started_ago_secs: i64, title: &str) -> Event {
        let mut event = online_event(calendar_id, -started_ago_secs);
        event.title = title.into();
        event
    }

    #[test]
    fn now_answers_with_the_meeting_in_progress() {
        let (svc, vault, _dir) = env();
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let event = running_event(calendar.id, 600, "Design sync");
        seed_event(&vault, calendar.id, &event);

        let answer = now(&svc, &vault).unwrap();

        assert!(answer.offers);
        assert!(answer.ready);
        assert!(answer.mic_offer);
        let offer = answer.event.expect("the meeting on now");
        assert_eq!(offer.event_id, event.id);
        assert_eq!(offer.calendar_name, "Work");
    }

    #[test]
    fn now_ignores_a_meeting_that_has_ended() {
        let (svc, vault, _dir) = env();
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        // Started an hour ago, ran half an hour.
        let over = running_event(calendar.id, 3_600, "Over");
        seed_event(&vault, calendar.id, &over);

        let answer = now(&svc, &vault).unwrap();

        assert!(answer.offers);
        assert!(answer.event.is_none());
    }

    #[test]
    fn now_prefers_the_meeting_that_started_most_recently() {
        let (svc, vault, _dir) = env();
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let earlier = running_event(calendar.id, 1_200, "Earlier");
        seed_event(&vault, calendar.id, &earlier);
        let later = running_event(calendar.id, 300, "Later");
        seed_event(&vault, calendar.id, &later);

        let answer = now(&svc, &vault).unwrap();

        assert_eq!(answer.event.expect("one of the two").title, "Later");
    }

    #[test]
    fn now_has_no_event_when_offers_are_off() {
        let (svc, vault, _dir) = env();
        enable(&vault, Offer::Off);
        let calendar = account_calendar(&vault);
        let event = running_event(calendar.id, 600, "Design sync");
        seed_event(&vault, calendar.id, &event);

        let answer = now(&svc, &vault).unwrap();

        assert!(!answer.offers);
        assert!(answer.event.is_none());
    }

    #[test]
    fn now_for_a_vault_never_set_up_is_not_ready() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = running_event(calendar.id, 600, "Design sync");
        seed_event(&vault, calendar.id, &event);

        let answer = now(&svc, &vault).unwrap();

        assert!(answer.offers);
        assert!(!answer.ready);
        assert!(answer.event.is_some_and(|o| !o.ready && !o.automatic));
    }

    /// `now` is a read. If it marked the event seen, the watcher's own tick
    /// -- which is what raises the offer for somebody not using the
    /// microphone at all -- would go quiet for the rest of the session.
    #[test]
    fn now_does_not_mark_the_event_offered() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);
        let event = online_event(calendar.id, 0);
        seed_event(&vault, calendar.id, &event);

        assert!(now(&svc, &vault).unwrap().event.is_some());
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

        dismiss(&svc, &vault, calendar.id, &event.uid, None, false).unwrap();
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

        dismiss(&svc, &vault, calendar.id, &event.uid, None, true).unwrap();

        assert!(
            vault
                .meeting_settings()
                .unwrap()
                .skipped_series
                .contains(&detect::series_key(&event.uid))
        );
    }

    /// The finding this guards: a recurring Google (`singleEvents=true`) or
    /// Graph (`calendarView`) event hands back a fresh `uid` for every
    /// occurrence, so keying "never for this meeting" on `uid` alone only
    /// ever skipped the one occurrence it was pressed on. Once the adapter
    /// populates `Event::series` with the durable series id, dismissing one
    /// occurrence -- carrying that same `series` on the offer, exactly as
    /// `MeetingOffer::series` does -- must also cover a later occurrence
    /// whose `uid` is different but whose `series` is the same.
    #[test]
    fn dismissing_never_with_a_series_id_covers_a_later_occurrence_with_a_different_uid() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, Offer::Ask);
        let calendar = account_calendar(&vault);

        let mut first = online_event(calendar.id, 0);
        first.uid = "google-instance-1".into();
        first.series = Some("google-series-abc".into());

        dismiss(&svc, &vault, calendar.id, &first.uid, first.series.as_deref(), true).unwrap();

        let mut second = online_event(calendar.id, 0);
        second.uid = "google-instance-2".into();
        second.series = Some("google-series-abc".into());
        seed_event(&vault, calendar.id, &second);

        tick(&svc, &vault);

        assert!(
            sink.offers.lock().unwrap().is_empty(),
            "a later occurrence of the same series must stay skipped"
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

        dismiss(&svc, &vault, calendar.id, uid, None, true).unwrap();

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

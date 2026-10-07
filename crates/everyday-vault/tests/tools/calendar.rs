//! Events, through the tools a model actually has: where a new one goes,
//! what it repeats as, and when a call stops to ask because it would email
//! somebody.

use std::cell::RefCell;

use everyday_core::account::{Account, AuthMethod, Provider};
use everyday_core::agent::tools::{self, CalendarWriter, Effect, ToolContext};
use everyday_core::calendar::{
    AccountCalendarSource, Attendee, Calendar, EditableEvent, Event, EventDraft, EventScope,
    EventStatus,
};
use everyday_core::error::Result;
use everyday_core::id::{CalendarId, EventId};
use everyday_core::recurrence::Frequency;
use everyday_core::store::tasks::BlockQuery;
use serde_json::json;

use super::support::{call, call_err, ctx, vault};

/// What a fake server was asked to do.
#[derive(Debug, Clone)]
enum Sent {
    Create(CalendarId, EventDraft),
    Update(EventId, EventDraft, EventScope),
    Delete(EventId, EventScope),
}

/// Stands in for Google, Graph or a CalDAV server: records every write,
/// and answers `load` with whatever event it was given.
#[derive(Default)]
struct FakeServer {
    sent: RefCell<Vec<Sent>>,
    loaded: Option<EditableEvent>,
}

impl CalendarWriter for FakeServer {
    fn create(&self, calendar: CalendarId, draft: &EventDraft) -> Result<Option<Event>> {
        self.sent.borrow_mut().push(Sent::Create(calendar, draft.clone()));
        Ok(None)
    }
    fn load(&self, _event: EventId) -> Result<EditableEvent> {
        Ok(self.loaded.clone().expect("the test gave the fake server an event to load"))
    }
    fn update(&self, event: EventId, draft: &EventDraft, scope: EventScope) -> Result<()> {
        self.sent.borrow_mut().push(Sent::Update(event, draft.clone(), scope));
        Ok(())
    }
    fn delete(&self, event: EventId, scope: EventScope) -> Result<()> {
        self.sent.borrow_mut().push(Sent::Delete(event, scope));
        Ok(())
    }
}

/// A Google calendar on an account signed in to write calendars.
fn a_google_calendar(vault: &everyday_core::Vault, name: &str) -> Calendar {
    let mut account = Account::new(Provider::Google, "me@gmail.com");
    account.services.calendar = true;
    if let AuthMethod::OAuth { scopes, .. } = &mut account.auth {
        scopes.extend(Provider::Google.preset().oauth.unwrap().calendar_scopes);
    }
    vault.save_account(&account).unwrap();
    let calendar = Calendar::from_account(
        account.id,
        Provider::Google,
        AccountCalendarSource::Google,
        "primary",
        name,
    );
    vault.save_calendar(&calendar).unwrap();
    calendar
}

/// An event as a sync would have stored it, with guests.
fn a_synced_event(vault: &everyday_core::Vault, calendar: &Calendar, guests: &[&str]) -> Event {
    let start: jiff::Timestamp = "2026-09-10T09:00:00Z".parse().unwrap();
    let event = Event {
        id: EventId::new(),
        calendar_id: calendar.id,
        uid: "abc123".into(),
        title: "Planning".into(),
        description: String::new(),
        location: String::new(),
        start,
        end: start + jiff::SignedDuration::from_hours(1),
        local_date: jiff::civil::date(2026, 9, 10),
        end_date: jiff::civil::date(2026, 9, 10),
        tz: "UTC".into(),
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: "me@gmail.com".into(),
        attendees: guests.iter().map(|g| g.to_string()).collect(),
        url: String::new(),
        busy: true,
        series: None,
        updated_at: start,
    };
    vault
        .sync_account_calendar(calendar.id, std::slice::from_ref(&event), &[], Default::default())
        .unwrap();
    event
}

#[test]
fn with_no_default_a_new_event_is_planned_time_on_this_computer() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let listed = call(&vault, "list_calendars", json!({}));
    assert_eq!(listed["default"], "this_computer");

    let made = call(
        &vault,
        "create_event",
        json!({
            "title": "Dentist", "date": "2026-09-09", "start_time": "15:00",
            "location": "High Street", "description": "Bring the form",
        }),
    );
    assert_eq!(made["kind"], "time block");
    let blocks = vault
        .blocks(&BlockQuery::between(jiff::civil::date(2026, 9, 9), jiff::civil::date(2026, 9, 9)))
        .unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].title, "Dentist \u{2014} High Street", "where it is rides in the title");
    assert_eq!(blocks[0].minutes(), 60, "an hour unless the end is named");
    assert_eq!(blocks[0].notes, "Bring the form");

    // Guests need a server to invite them.
    let err = call_err(
        &vault,
        "create_event",
        json!({
            "title": "Lunch", "date": "2026-09-09", "start_time": "12:00",
            "attendees": ["sam@example.com"],
        }),
    );
    assert!(err.contains("account's calendar"), "got {err}");
}

#[test]
fn a_repeating_event_on_this_computer_is_written_out_as_a_series() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let made = call(
        &vault,
        "create_event",
        json!({
            "title": "Gym", "date": "2026-09-14", "start_time": "07:00", "end_time": "08:00",
            "repeat": { "frequency": "weekly", "weekdays": ["Mon", "thursday"], "count": 6 },
        }),
    );
    assert_eq!(made["occurrences_written"], 6);
    assert_eq!(made["repeats"], "every week on Monday and Thursday, 6 times");
    let blocks = vault
        .blocks(&BlockQuery::between(
            jiff::civil::date(2026, 9, 1),
            jiff::civil::date(2026, 12, 31),
        ))
        .unwrap();
    assert_eq!(blocks.len(), 6);
    assert!(blocks.iter().all(|b| b.series.is_some()));

    let err = call_err(
        &vault,
        "create_event",
        json!({
            "title": "Odd", "date": "2026-09-14", "start_time": "07:00",
            "repeat": { "frequency": "daily", "week_of_month": 2 },
        }),
    );
    assert!(err.contains("week_of_month"), "got {err}");
}

#[test]
fn the_default_calendar_takes_new_events_and_the_server_is_handed_the_whole_draft() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let work = a_google_calendar(&vault, "Work");
    vault.set_default_calendar(Some(work.id)).unwrap();

    let listed = call(&vault, "list_calendars", json!({}));
    assert_eq!(listed["default"], work.id.to_string());

    let server = FakeServer::default();
    let ctx = ToolContext { calendar_writer: Some(&server), ..ctx(&vault) };
    let made = tools::dispatch(
        &ctx,
        "create_event",
        &json!({
            "title": "Design review", "date": "2026-09-15", "start_time": "10:00",
            "end_time": "11:30", "description": "Agenda in the doc",
            "attendees": ["Sam Lee <sam@example.com>", "kim@example.com"],
            "repeat": { "frequency": "monthly", "week_of_month": 3, "weekdays": ["tuesday"] },
        }),
    )
    .unwrap();
    assert_eq!(made["kind"], "event");
    assert_eq!(made["calendar"], "Work");

    let sent = server.sent.borrow();
    let Some(Sent::Create(calendar, draft)) = sent.first() else { panic!("nothing created") };
    assert_eq!(*calendar, work.id);
    assert_eq!(draft.minutes(), 90);
    assert_eq!(draft.description, "Agenda in the doc");
    assert_eq!(
        draft.attendees,
        vec![
            Attendee { email: "sam@example.com".into(), name: "Sam Lee".into(), response: None },
            Attendee::new("kim@example.com"),
        ],
    );
    let rule = draft.recurrence.as_ref().unwrap();
    assert_eq!(rule.frequency, Frequency::Monthly);
    assert_eq!(rule.to_rrule(false, "UTC"), "FREQ=MONTHLY;BYDAY=3TU");

    // Naming this computer overrides the default.
    let local = tools::dispatch(
        &ctx,
        "create_event",
        &json!({ "title": "Read", "date": "2026-09-15", "start_time": "20:00",
                 "calendar_id": "this_computer" }),
    )
    .unwrap();
    assert_eq!(local["kind"], "time block");
}

#[test]
fn only_a_call_that_reaches_people_has_to_ask_first() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let work = a_google_calendar(&vault, "Work");
    let ctx = ctx(&vault);

    let alone = json!({ "title": "Focus", "date": "2026-09-15", "start_time": "09:00" });
    assert_eq!(tools::effect_for(&ctx, "create_event", &alone), Some(Effect::Write));
    let invited = json!({ "title": "Sync", "date": "2026-09-15", "start_time": "09:00",
                          "attendees": ["sam@example.com"] });
    assert_eq!(tools::effect_for(&ctx, "create_event", &invited), Some(Effect::Outward));
    let card = tools::describe(&ctx, "create_event", &invited).unwrap();
    assert!(card.contains("sam@example.com"), "the card names who is invited: {card}");

    let quiet = a_synced_event(&vault, &work, &[]);
    let busy = a_synced_event(&vault, &work, &["Sam Lee"]);
    let delete = |id: EventId| json!({ "event_id": id.to_string() });
    assert_eq!(
        tools::effect_for(&ctx, "delete_event", &delete(quiet.id)),
        Some(Effect::Destructive)
    );
    assert_eq!(tools::effect_for(&ctx, "delete_event", &delete(busy.id)), Some(Effect::Outward));
    let rename = json!({ "event_id": quiet.id.to_string(), "title": "x" });
    assert_eq!(tools::effect_for(&ctx, "update_event", &rename), Some(Effect::Write));
    let invite = json!({ "event_id": quiet.id.to_string(), "add_attendees": ["a@b.co"] });
    assert_eq!(tools::effect_for(&ctx, "update_event", &invite), Some(Effect::Outward));

    // A scheduled run cannot send invitations at all.
    let unattended = ToolContext { unattended: true, ..ctx };
    let err = tools::dispatch(&unattended, "create_event", &invited).unwrap_err().to_string();
    assert!(err.contains("scheduled run"), "got {err}");
}

#[test]
fn an_update_names_only_what_changes_and_a_new_rule_means_the_series() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let work = a_google_calendar(&vault, "Work");
    let event = a_synced_event(&vault, &work, &["sam@example.com"]);
    let server = FakeServer {
        loaded: Some(EditableEvent {
            event_id: event.id,
            calendar_id: work.id,
            draft: EventDraft {
                title: "Planning".into(),
                description: "Long agenda".into(),
                location: String::new(),
                start: event.start,
                end: event.end,
                all_day: false,
                tz: "UTC".into(),
                attendees: vec![Attendee::new("sam@example.com")],
                recurrence: Some(everyday_core::recurrence::Recurrence::every(Frequency::Weekly)),
            },
            recurring: true,
            custom_recurrence: false,
            own: true,
            organizer: "me@gmail.com".into(),
        }),
        ..Default::default()
    };
    let ctx = ToolContext { calendar_writer: Some(&server), ..ctx(&vault) };
    tools::dispatch(
        &ctx,
        "update_event",
        &json!({
            "event_id": event.id.to_string(),
            "start_time": "10:00",
            "add_attendees": ["kim@example.com"],
            "remove_attendees": ["SAM@example.com"],
            "repeat": { "frequency": "weekly", "interval": 2 },
        }),
    )
    .unwrap();
    let sent = server.sent.borrow();
    let Some(Sent::Update(id, draft, scope)) = sent.first() else { panic!("nothing updated") };
    assert_eq!(*id, event.id);
    assert_eq!(*scope, EventScope::Series, "changing the rule changes the series");
    assert_eq!(draft.description, "Long agenda", "what was not named stays");
    assert_eq!(draft.attendees, vec![Attendee::new("kim@example.com")]);
    assert_eq!(draft.minutes(), 60, "moving keeps the length");
    assert_eq!(draft.start, "2026-09-10T10:00:00Z".parse::<jiff::Timestamp>().unwrap());
    assert_eq!(draft.recurrence.as_ref().unwrap().interval, 2);
    drop(sent);

    // Somebody else's invitation is theirs to change.
    let theirs = FakeServer {
        loaded: server.loaded.clone().map(|e| EditableEvent {
            own: false,
            organizer: "Pat".into(),
            ..e
        }),
        ..Default::default()
    };
    let ctx = ToolContext { calendar_writer: Some(&theirs), ..ctx };
    let err = tools::dispatch(
        &ctx,
        "update_event",
        &json!({ "event_id": event.id.to_string(), "title": "Mine now" }),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("Pat"), "got {err}");
    assert!(theirs.sent.borrow().is_empty());
}

#[test]
fn a_deletion_names_the_event_and_the_part_of_its_series_it_means() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let work = a_google_calendar(&vault, "Work");
    let event = a_synced_event(&vault, &work, &[]);
    let server = FakeServer::default();
    let ctx = ToolContext { calendar_writer: Some(&server), ..ctx(&vault) };

    let card = tools::describe(
        &ctx,
        "delete_event",
        &json!({ "event_id": event.id.to_string(), "scope": "series" }),
    )
    .unwrap();
    assert!(card.contains("Planning") && card.contains("every other occurrence"), "{card}");

    let gone = tools::dispatch(
        &ctx,
        "delete_event",
        &json!({ "event_id": event.id.to_string(), "scope": "series" }),
    )
    .unwrap();
    assert_eq!(gone["name"], "Planning");
    let sent = server.sent.borrow();
    let Some(Sent::Delete(id, scope)) = sent.first() else { panic!("nothing deleted") };
    assert_eq!((*id, *scope), (event.id, EventScope::Series));
}

#[test]
fn a_read_only_calendar_is_refused_with_what_to_do_instead() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let feed = Calendar::subscribed("Holidays", "https://example.com/h.ics");
    vault.save_calendar(&feed).unwrap();
    let err = call_err(
        &vault,
        "create_event",
        json!({ "title": "x", "date": "2026-09-09", "start_time": "09:00",
                "calendar_id": feed.id.to_string() }),
    );
    assert!(err.contains("read-only"), "got {err}");

    // A Google calendar whose account only ever granted reading.
    let mut account = Account::new(Provider::Google, "old@gmail.com");
    account.services.calendar = true;
    vault.save_account(&account).unwrap();
    let old = Calendar::from_account(
        account.id,
        Provider::Google,
        AccountCalendarSource::Google,
        "primary",
        "Old",
    );
    vault.save_calendar(&old).unwrap();
    let err = call_err(
        &vault,
        "create_event",
        json!({ "title": "x", "date": "2026-09-09", "start_time": "09:00",
                "calendar_id": old.id.to_string() }),
    );
    assert!(err.contains("signed in again"), "got {err}");
}

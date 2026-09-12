//! The calendar half of the conformance suite: subscriptions and events.

use super::*;

/// The calendar half of the suite. Called by [`run_all`] when the backend
/// has a [`CalendarStore`]; public so a backend under construction can run
/// it alone.
///
/// The store must be empty of calendars on entry; it is left empty on
/// success.
pub fn run_calendar_suite(store: &dyn CalendarStore) {
    eprintln!("--- calendar conformance suite ---");

    calendars_start_empty(store);
    calendar_crud(store);
    missing_calendar_records_are_not_found(store);
    events_round_trip_every_field(store);
    replacing_events_is_total_and_scoped_to_one_calendar(store);
    listing_events_filters_by_window_and_text(store);
    deleting_a_calendar_takes_its_events_with_it(store);
    unicode_survives_a_calendar_round_trip(store);

    calendar_cleanup(store);
    eprintln!("--- calendar suite passed ---");
}

fn calendar_cleanup(store: &dyn CalendarStore) {
    for c in store.list_calendars().expect("list_calendars") {
        store.delete_calendar(c.id).expect("delete_calendar");
    }
    assert!(store.list_calendars().unwrap().is_empty(), "cleanup left calendars behind");
    assert!(
        store.list_events(&EventQuery::default()).unwrap().is_empty(),
        "cleanup left events behind"
    );
}

fn seeded_calendar(store: &dyn CalendarStore, name: &str) -> Calendar {
    let c = Calendar::subscribed(name, format!("https://example.com/{name}.ics"));
    store.put_calendar(&c).expect("put_calendar");
    c
}

/// An event on `cal` covering `from..=to`, at 09:00 UTC on the first day.
pub(super) fn sample_event(cal: CalendarId, uid: &str, from: Date, to: Date) -> Event {
    let start: Timestamp = format!("{from}T09:00:00Z").parse().expect("start");
    let end: Timestamp = format!("{to}T10:00:00Z").parse().expect("end");
    Event {
        id: EventId::new(),
        calendar_id: cal,
        uid: uid.to_string(),
        title: uid.to_string(),
        description: String::new(),
        location: String::new(),
        start,
        end,
        local_date: from,
        end_date: to,
        tz: "UTC".into(),
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: String::new(),
        attendees: Vec::new(),
        url: String::new(),
        busy: true,
        updated_at: Timestamp::now(),
    }
}

fn calendars_start_empty(store: &dyn CalendarStore) {
    assert!(store.list_calendars().unwrap().is_empty(), "a fresh store must have no calendars");
    assert!(
        store.list_events(&EventQuery::default()).unwrap().is_empty(),
        "a fresh store must have no events"
    );
}

fn calendar_crud(store: &dyn CalendarStore) {
    let mut cal = Calendar::subscribed("Work", "webcal://example.com/work.ics");
    cal.color = "#0f766e".into();
    cal.refresh_minutes = 30;
    cal.visible = false;
    store.put_calendar(&cal).expect("put_calendar");

    let back = store.get_calendar(cal.id).expect("get_calendar");
    assert_eq!(back, cal, "a calendar must survive the round trip unchanged");
    assert_eq!(store.list_calendars().unwrap().len(), 1);

    // Idempotent: writing the same id twice updates rather than duplicates.
    cal.name = "Work (renamed)".into();
    cal.mark_synced();
    store.put_calendar(&cal).expect("put_calendar again");
    assert_eq!(store.list_calendars().unwrap().len(), 1, "put must not duplicate");
    let back = store.get_calendar(cal.id).expect("get_calendar");
    assert_eq!(back.name, "Work (renamed)");
    assert!(back.last_synced_at.is_some(), "the sync stamp must persist");

    store.delete_calendar(cal.id).expect("delete_calendar");
    assert!(store.list_calendars().unwrap().is_empty());
}

fn missing_calendar_records_are_not_found(store: &dyn CalendarStore) {
    super::assert_not_found(store.get_calendar(CalendarId::new()));
    super::assert_not_found(store.get_event(EventId::new()));
    assert_eq!(
        store.count_events(CalendarId::new()).expect("count_events"),
        0,
        "a calendar that does not exist holds no events, and asking is not an error",
    );
}

fn events_round_trip_every_field(store: &dyn CalendarStore) {
    let cal = seeded_calendar(store, "round-trip");
    let mut event = sample_event(cal.id, "uid-1", date(2026, 4, 8), date(2026, 4, 8));
    event.title = "Quarterly review".into();
    event.description = "Bring the numbers.\nAll of them.".into();
    event.location = "Room 4, second floor".into();
    event.organizer = "Priya Raman".into();
    event.url = "https://example.com/meet/abc".into();
    event.status = EventStatus::Tentative;
    event.busy = false;
    event.all_day = true;
    event.tz = "Europe/Berlin".into();

    store.replace_events(cal.id, std::slice::from_ref(&event)).expect("replace_events");

    let back = store.get_event(event.id).expect("get_event");
    assert_eq!(back, event, "an event must survive the round trip unchanged");
    assert_eq!(store.count_events(cal.id).expect("count_events"), 1);

    store.delete_calendar(cal.id).expect("delete_calendar");
}

fn replacing_events_is_total_and_scoped_to_one_calendar(store: &dyn CalendarStore) {
    let mine = seeded_calendar(store, "mine");
    let theirs = seeded_calendar(store, "theirs");

    let first: Vec<Event> = (1..=3)
        .map(|d| sample_event(mine.id, &format!("a{d}"), date(2026, 4, d), date(2026, 4, d)))
        .collect();
    let others: Vec<Event> =
        vec![sample_event(theirs.id, "b1", date(2026, 4, 1), date(2026, 4, 1))];
    store.replace_events(mine.id, &first).expect("replace_events");
    store.replace_events(theirs.id, &others).expect("replace_events");
    assert_eq!(store.count_events(mine.id).unwrap(), 3);

    // A second sync of one feed replaces that feed's set entirely...
    let second: Vec<Event> = vec![sample_event(mine.id, "a9", date(2026, 4, 9), date(2026, 4, 9))];
    store.replace_events(mine.id, &second).expect("replace_events again");
    assert_eq!(store.count_events(mine.id).unwrap(), 1, "a sync replaces, it does not append");
    let rows = store.list_events(&EventQuery { calendar_id: Some(mine.id), ..Default::default() });
    assert_eq!(rows.unwrap()[0].uid, "a9");

    // ...and leaves every other calendar alone.
    assert_eq!(store.count_events(theirs.id).unwrap(), 1, "one feed must not clear another");

    // Replacing with nothing empties it, which is what an emptied calendar
    // upstream should look like.
    store.replace_events(mine.id, &[]).expect("replace_events with none");
    assert_eq!(store.count_events(mine.id).unwrap(), 0);
    assert_eq!(store.count_events(theirs.id).unwrap(), 1);

    store.delete_calendar(mine.id).expect("delete_calendar");
    store.delete_calendar(theirs.id).expect("delete_calendar");
}

fn listing_events_filters_by_window_and_text(store: &dyn CalendarStore) {
    let cal = seeded_calendar(store, "window");
    let mut trip = sample_event(cal.id, "trip", date(2026, 7, 10), date(2026, 7, 20));
    trip.title = "Lisbon".into();
    trip.location = "Praça do Comércio".into();
    let mut lunch = sample_event(cal.id, "lunch", date(2026, 7, 30), date(2026, 7, 30));
    lunch.title = "Lunch".into();
    let mut old = sample_event(cal.id, "old", date(2026, 1, 5), date(2026, 1, 5));
    old.title = "Kickoff".into();
    store.replace_events(cal.id, &[trip.clone(), lunch.clone(), old]).expect("replace_events");

    // The window test is *overlap*, not "starts inside": a fortnight away
    // must still be found in the middle week of it.
    let mid = store
        .list_events(&EventQuery::between(date(2026, 7, 13), date(2026, 7, 19)))
        .expect("list_events");
    assert_eq!(
        mid.iter().map(|e| e.uid.as_str()).collect::<Vec<_>>(),
        ["trip"],
        "a week query must find an event that started before it",
    );

    let july = store
        .list_events(&EventQuery::between(date(2026, 7, 1), date(2026, 7, 31)))
        .expect("list_events");
    assert_eq!(july.len(), 2);
    assert!(july[0].start <= july[1].start, "events come back in time order");

    let by_text = store
        .list_events(&EventQuery { text: "comércio".into(), ..Default::default() })
        .expect("list_events");
    assert_eq!(by_text.len(), 1, "text search covers the location and ignores case");

    let capped = store
        .list_events(&EventQuery { limit: Some(1), ..Default::default() })
        .expect("list_events");
    assert_eq!(capped.len(), 1);

    store.delete_calendar(cal.id).expect("delete_calendar");
}

fn deleting_a_calendar_takes_its_events_with_it(store: &dyn CalendarStore) {
    let doomed = seeded_calendar(store, "doomed");
    let kept = seeded_calendar(store, "kept");
    store
        .replace_events(
            doomed.id,
            &[sample_event(doomed.id, "x", date(2026, 5, 1), date(2026, 5, 1))],
        )
        .expect("replace_events");
    store
        .replace_events(kept.id, &[sample_event(kept.id, "y", date(2026, 5, 1), date(2026, 5, 1))])
        .expect("replace_events");

    store.delete_calendar(doomed.id).expect("delete_calendar");
    assert!(store.get_calendar(doomed.id).is_err());
    assert_eq!(
        store.count_events(doomed.id).unwrap(),
        0,
        "unsubscribing must not leave its events behind",
    );
    assert_eq!(store.count_events(kept.id).unwrap(), 1, "and must not take anyone else's");

    store.delete_calendar(kept.id).expect("delete_calendar");
}

fn unicode_survives_a_calendar_round_trip(store: &dyn CalendarStore) {
    let mut cal = Calendar::subscribed("日本の祝日 🎌", "https://example.com/jp.ics");
    cal.color = "#be123c".into();
    store.put_calendar(&cal).expect("put_calendar");

    let mut event = sample_event(cal.id, "unicode", date(2026, 5, 5), date(2026, 5, 5));
    event.title = "こどもの日 — Children\u{2019}s Day".into();
    event.location = "全国".into();
    store.replace_events(cal.id, std::slice::from_ref(&event)).expect("replace_events");

    assert_eq!(store.get_calendar(cal.id).unwrap().name, "日本の祝日 🎌");
    assert_eq!(store.get_event(event.id).unwrap().title, event.title);

    store.delete_calendar(cal.id).expect("delete_calendar");
}

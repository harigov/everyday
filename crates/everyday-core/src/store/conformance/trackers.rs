//! The tracking half of the conformance suite: definitions and readings.

use super::*;

/// The tracking half of the suite. Called by [`run_all`] when the backend
/// has a [`TrackerStore`](crate::store::trackers::TrackerStore).
///
/// This did not exist until trackers became records of their own: while a
/// definition was a field inside a journal there was only half a domain to
/// check, and the readings were tested per backend instead. Both halves are
/// storage now, so both are checked here, once, for every backend.
///
/// The store must be empty of trackers on entry; it is left empty on success.
pub fn run_tracker_suite(store: &dyn JournalStore) {
    eprintln!("--- tracking conformance suite ---");

    tracking_starts_empty(store);
    tracker_crud(store);
    missing_tracking_records_are_not_found(store);
    reading_round_trips_every_field(store);
    readings_filter_and_aggregate_over_the_same_window(store);
    a_reading_need_not_name_a_journal_or_an_entry(store);
    deleting_a_tracker_takes_its_readings_and_nothing_else(store);
    merging_a_tracker_keeps_both_histories(store);
    deleting_a_journal_detaches_its_readings_rather_than_deleting_them(store);
    deleting_an_entry_keeps_its_readings(store);
    a_tracker_can_measure_a_goal(store);
    unicode_survives_a_tracking_round_trip(store);

    tracking_cleanup(store);
    eprintln!("--- tracking suite passed ---");
}

fn tracker_store(store: &dyn JournalStore) -> &dyn crate::store::trackers::TrackerStore {
    store.trackers().expect("the tracking suite needs a tracker store")
}

fn tracking_cleanup(store: &dyn JournalStore) {
    let t = tracker_store(store);
    for tracker in t.list_trackers().expect("list_trackers") {
        t.delete_tracker(tracker.id).expect("delete_tracker");
    }
    assert!(t.list_trackers().unwrap().is_empty(), "cleanup left trackers behind");
    assert!(
        t.list_readings(&ReadingQuery::default()).unwrap().is_empty(),
        "cleanup left readings behind"
    );
}

fn seeded_tracker(store: &dyn JournalStore, name: &str, kind: TrackerKind) -> Tracker {
    let tracker = Tracker::new(name, kind);
    tracker_store(store).put_tracker(&tracker).expect("put_tracker");
    tracker
}

fn tracking_starts_empty(store: &dyn JournalStore) {
    let t = tracker_store(store);
    assert!(t.list_trackers().unwrap().is_empty(), "a fresh store must have no trackers");
    assert!(
        t.list_readings(&ReadingQuery::default()).unwrap().is_empty(),
        "a fresh store must have no readings"
    );
}

fn tracker_crud(store: &dyn JournalStore) {
    let t = tracker_store(store);
    let mut tracker =
        Tracker::new("Sertraline", TrackerKind::Dose).with_unit("mg").every(1, Period::Day);
    tracker.icon = "pill".into();
    tracker.color = "#0f766e".into();
    tracker.default_value = 50.0;
    tracker.target = Some(50.0);
    tracker.on_calendar = true;
    tracker.sort_order = 2;
    t.put_tracker(&tracker).unwrap();

    assert_eq!(t.get_tracker(tracker.id).unwrap(), tracker, "every field must survive");
    assert_eq!(t.list_trackers().unwrap().len(), 1);

    // Idempotent, as every `put_` in this codebase is.
    t.put_tracker(&tracker).unwrap();
    assert_eq!(t.list_trackers().unwrap().len(), 1, "putting twice must not make two");

    tracker.archived = true;
    tracker.cadence = Cadence::new(3, Period::Week);
    t.put_tracker(&tracker).unwrap();
    let back = t.get_tracker(tracker.id).unwrap();
    assert!(back.archived, "an archived tracker is still stored; it only leaves the page");
    assert_eq!(back.cadence, Cadence::new(3, Period::Week));
    assert_eq!(t.list_trackers().unwrap().len(), 1, "archiving is not deleting");

    t.delete_tracker(tracker.id).unwrap();
    assert!(t.list_trackers().unwrap().is_empty());
}

fn missing_tracking_records_are_not_found(store: &dyn JournalStore) {
    let t = tracker_store(store);
    // A tracker that was never written is not found, not a default one.
    super::assert_not_found(t.get_tracker(TrackerId::new()));
    super::assert_not_found(t.get_reading(ReadingId::new()));
    // Deleting what is not there is not an error: it is already true.
    assert_eq!(t.delete_tracker(TrackerId::new()).unwrap(), 0);
    t.delete_reading(ReadingId::new()).expect("deleting a missing reading is a no-op");
}

fn reading_round_trips_every_field(store: &dyn JournalStore) {
    let t = tracker_store(store);
    let tracker = seeded_tracker(store, "Headache", TrackerKind::Scale);
    let journal = Journal::new("tracking");
    store.put_journal(&journal).unwrap();
    let entry = Entry::new(journal.id, "UTC");
    store.put_entry(&entry).unwrap();

    let at: Timestamp = "2026-03-14T08:12:00Z".parse().unwrap();
    let mut reading = Reading::at(tracker.id, at, "Europe/Berlin", 6.0)
        .in_journal(journal.id)
        .with_entry(entry.id);
    reading.note = "behind the left eye".into();
    t.put_reading(&reading).unwrap();

    assert_eq!(t.get_reading(reading.id).unwrap(), reading);

    // The day and the minute have to agree after a round trip, or a chip
    // ticked at 00:10 lands on the calendar a day from its entry.
    let back = t.get_reading(reading.id).unwrap();
    assert_eq!(back.at, Some(at));
    assert_eq!(back.local_date, crate::model::local_date_in(at, "Europe/Berlin"));

    store.delete_journal(journal.id).unwrap();
    t.delete_tracker(tracker.id).unwrap();
}

fn readings_filter_and_aggregate_over_the_same_window(store: &dyn JournalStore) {
    // `list_readings` and `tracker_days` must cover the same rows: a chart
    // whose totals came from a different set than the list beneath it is a
    // bug nobody can see.
    let t = tracker_store(store);
    let pain = seeded_tracker(store, "Pain", TrackerKind::Scale);
    let dose = seeded_tracker(store, "Ibuprofen", TrackerKind::Dose);

    for (day, value) in [(1, 3.0), (1, 7.0), (2, 4.0), (9, 8.0)] {
        t.put_reading(&Reading::on(pain.id, date(2026, 3, day), value)).unwrap();
    }
    t.put_reading(&Reading::on(dose.id, date(2026, 3, 1), 400.0)).unwrap();

    let window = ReadingQuery {
        from: Some(date(2026, 3, 1)),
        to: Some(date(2026, 3, 2)),
        ..Default::default()
    };
    let listed = t.list_readings(&window).unwrap();
    assert_eq!(listed.len(), 4, "three pain readings and one dose fall in the window");

    let days = t.tracker_days(&window).unwrap();
    let first = days
        .iter()
        .find(|d| d.tracker_id == pain.id && d.date == date(2026, 3, 1))
        .expect("a row for the first day");
    assert_eq!(first.count, 2);
    // Two headaches, a 3 and a 7: the day averaged 5 and was never a 10.
    assert_eq!(first.value_for(Aggregate::Mean), 5.0);
    assert_eq!(first.value_for(Aggregate::Sum), 10.0);
    assert_eq!(first.max, 7.0);

    // A tracker filter narrows both halves alike.
    let just_pain = ReadingQuery { tracker_ids: vec![pain.id], ..window.clone() };
    assert_eq!(t.list_readings(&just_pain).unwrap().len(), 3);
    assert_eq!(t.tracker_days(&just_pain).unwrap().len(), 2);

    // A cap applies to the list and never to the aggregate: a chart that
    // paginated would be a chart that lied.
    let capped = ReadingQuery { limit: Some(1), ..just_pain.clone() };
    assert_eq!(t.list_readings(&capped).unwrap().len(), 1);
    assert_eq!(
        t.tracker_days(&capped).unwrap().iter().map(|d| d.count).sum::<u32>(),
        3,
        "the aggregate must ignore a limit meant for a list"
    );

    t.delete_tracker(pain.id).unwrap();
    t.delete_tracker(dose.id).unwrap();
}

fn a_reading_need_not_name_a_journal_or_an_entry(store: &dyn JournalStore) {
    // What quick-track from the Overview produces. It used to be impossible:
    // a tracker was a field inside one journal, so every reading had one.
    let t = tracker_store(store);
    let tracker = seeded_tracker(store, "Swimming", TrackerKind::Amount);
    let reading = Reading::on(tracker.id, date(2026, 4, 1), 60.0);
    t.put_reading(&reading).unwrap();

    let back = t.get_reading(reading.id).unwrap();
    assert_eq!(back.journal_id, None);
    assert_eq!(back.entry_id, None);
    assert_eq!(back.value, 60.0);

    // A journal filter must not sweep it in. An unfiled reading is outside
    // every journal, not inside all of them.
    let elsewhere = ReadingQuery { journal_id: Some(JournalId::new()), ..Default::default() };
    assert!(t.list_readings(&elsewhere).unwrap().is_empty());

    t.delete_tracker(tracker.id).unwrap();
}

fn deleting_a_tracker_takes_its_readings_and_nothing_else(store: &dyn JournalStore) {
    let t = tracker_store(store);
    let gone = seeded_tracker(store, "Doomed", TrackerKind::Check);
    let kept = seeded_tracker(store, "Kept", TrackerKind::Check);
    for day in [1, 2] {
        t.put_reading(&Reading::on(gone.id, date(2026, 5, day), 1.0)).unwrap();
    }
    t.put_reading(&Reading::on(kept.id, date(2026, 5, 1), 1.0)).unwrap();

    assert_eq!(t.delete_tracker(gone.id).unwrap(), 2, "it reports what it took");
    assert!(t.get_tracker(gone.id).is_err(), "the definition goes with them");

    let left = t.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].tracker_id, kept.id);

    t.delete_tracker(kept.id).unwrap();
}

fn merging_a_tracker_keeps_both_histories(store: &dyn JournalStore) {
    // The tidy-up lazy creation needs: `#swim` on Monday and `#swimming` on
    // Friday are two records of one thing, and the answer cannot be to throw
    // away a month of numbers.
    let t = tracker_store(store);
    let scruffy = seeded_tracker(store, "swim", TrackerKind::Amount);
    let proper = seeded_tracker(store, "Swimming", TrackerKind::Amount);

    for day in [1, 2, 3] {
        t.put_reading(&Reading::on(scruffy.id, date(2026, 6, day), 30.0)).unwrap();
    }
    let older = Reading::on(proper.id, date(2026, 6, 4), 45.0);
    t.put_reading(&older).unwrap();

    assert_eq!(t.merge_trackers(scruffy.id, proper.id).unwrap(), 3);
    assert!(t.get_tracker(scruffy.id).is_err(), "the one merged away is gone");

    let all = t.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(all.len(), 4, "no reading may be lost in a merge");
    assert!(
        all.iter().all(|r| r.tracker_id == proper.id),
        "every reading must point at the survivor -- in the sealed payload too"
    );
    // The clear column as well, or the index would still find them under a
    // tracker that no longer exists.
    let by_old = ReadingQuery { tracker_ids: vec![scruffy.id], ..Default::default() };
    assert!(t.list_readings(&by_old).unwrap().is_empty());

    t.delete_tracker(proper.id).unwrap();
}

fn deleting_a_journal_detaches_its_readings_rather_than_deleting_them(store: &dyn JournalStore) {
    let t = tracker_store(store);
    let tracker = seeded_tracker(store, "Steps", TrackerKind::Amount);
    let mine = Journal::new("doomed");
    let other = Journal::new("kept");
    store.put_journal(&mine).unwrap();
    store.put_journal(&other).unwrap();

    let here = Reading::on(tracker.id, date(2026, 7, 1), 9000.0).in_journal(mine.id);
    let there = Reading::on(tracker.id, date(2026, 7, 2), 8000.0).in_journal(other.id);
    t.put_reading(&here).unwrap();
    t.put_reading(&there).unwrap();

    store.delete_journal(mine.id).unwrap();

    // Both survive. A reading belongs to its tracker; the journal is only
    // where it happened to be ticked, and deleting the notebook you wrote in
    // does not undo the walk.
    let left = t.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(left.len(), 2, "readings outlive the journal they were logged in");
    assert_eq!(t.get_reading(here.id).unwrap().journal_id, None, "the sealed link is cleared");
    assert_eq!(t.get_reading(there.id).unwrap().journal_id, Some(other.id));

    // The clear column too, or the index would still find it by that journal.
    let by_journal = ReadingQuery { journal_id: Some(mine.id), ..Default::default() };
    assert!(t.list_readings(&by_journal).unwrap().is_empty());

    store.delete_journal(other.id).unwrap();
    t.delete_tracker(tracker.id).unwrap();
}

fn deleting_an_entry_keeps_its_readings(store: &dyn JournalStore) {
    // Deleting the paragraph about a run does not undo the run. What must
    // not survive is the pointer, in both copies of it.
    let t = tracker_store(store);
    let tracker = seeded_tracker(store, "Run", TrackerKind::Amount);
    let journal = Journal::new("running");
    store.put_journal(&journal).unwrap();
    let entry = Entry::new(journal.id, "UTC");
    store.put_entry(&entry).unwrap();

    let reading =
        Reading::on(tracker.id, date(2026, 8, 1), 45.0).in_journal(journal.id).with_entry(entry.id);
    t.put_reading(&reading).unwrap();

    store.delete_entry(entry.id).unwrap();

    let kept = t.get_reading(reading.id).unwrap();
    assert_eq!(kept.value, 45.0, "the reading itself must survive");
    assert_eq!(kept.entry_id, None, "the sealed copy of the link must be cleared");
    let by_entry = ReadingQuery { entry_id: Some(entry.id), ..Default::default() };
    assert!(t.list_readings(&by_entry).unwrap().is_empty());

    store.delete_journal(journal.id).unwrap();
    t.delete_tracker(tracker.id).unwrap();
}

fn a_tracker_can_measure_a_goal(store: &dyn JournalStore) {
    // The reason trackers left the journal at all: a habit can be the
    // evidence a goal is alive, and a goal with no work under it has no
    // other evidence.
    let Some(purpose) = store.purpose() else { return };
    let t = tracker_store(store);
    let role = LifeRole::new("Health");
    purpose.put_role(&role).unwrap();
    let goal = Goal::new(role.id, "run 10k without stopping");
    purpose.put_goal(&goal).unwrap();

    let mut tracker = Tracker::new("Run", TrackerKind::Amount).with_unit("min");
    tracker.purpose = Some(goal.purpose());
    tracker.cadence = Cadence::new(3, Period::Week);
    t.put_tracker(&tracker).unwrap();

    assert_eq!(t.get_tracker(tracker.id).unwrap().purpose, Some(goal.purpose()));

    for day in [1, 3, 5] {
        t.put_reading(&Reading::on(tracker.id, date(2026, 9, day), 30.0)).unwrap();
    }
    let activity = purpose.goal_activity(goal.id).unwrap();
    assert_eq!(activity.readings, 3, "a goal's tracker readings count as activity on it");
    assert!(activity.last_touched.is_some());

    // ...and stop counting when the tracker goes.
    t.delete_tracker(tracker.id).unwrap();
    assert_eq!(purpose.goal_activity(goal.id).unwrap().readings, 0);

    purpose.delete_goal(goal.id).unwrap();
    purpose.delete_role(role.id).unwrap();
}

fn unicode_survives_a_tracking_round_trip(store: &dyn JournalStore) {
    let t = tracker_store(store);
    let mut tracker = Tracker::new("頭痛 · головная боль 🤕", TrackerKind::Scale);
    tracker.unit = "\u{00b0}".into();
    t.put_tracker(&tracker).unwrap();
    assert_eq!(t.get_tracker(tracker.id).unwrap(), tracker);

    let mut reading = Reading::on(tracker.id, date(2026, 10, 1), 4.0);
    reading.note = "nach dem Mittagessen — links".into();
    t.put_reading(&reading).unwrap();
    assert_eq!(t.get_reading(reading.id).unwrap(), reading);

    t.delete_tracker(tracker.id).unwrap();
}

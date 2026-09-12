//! The rules the vault applies to trackers and their readings, over a real
//! SQLite backend: clamping, cascading deletes, archiving, merging, and the
//! one migration in the application that cannot be a SQL step.

mod support;
use support::{a_tracked, vault};

#[test]
fn a_reading_is_clamped_by_the_tracker_that_defines_it() {
    // The reason the vault looks the definition up rather than trusting
    // the caller: a severity of 99 on a scale of ten is a chart with an
    // axis to the moon and a thousand rows to search for the cause.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut pain = everyday_core::Tracker::new("Headache", everyday_core::TrackerKind::Scale);
    pain.scale_max = 10.0;
    let pain = a_tracked(&vault, pain);

    let reading =
        everyday_core::Reading::on(pain.id, jiff::civil::Date::constant(2026, 3, 14), 99.0);
    vault.save_reading(&reading).unwrap();
    assert_eq!(vault.reading(reading.id).unwrap().value, 10.0);
}

#[test]
fn a_reading_naming_no_tracker_is_refused() {
    // Otherwise it is an unnameable row: a number against an id that
    // nothing in the vault can turn back into a word.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let orphan = everyday_core::Reading::on(
        everyday_core::TrackerId::new(),
        jiff::civil::Date::constant(2026, 3, 14),
        1.0,
    );
    let err = vault.save_reading(&orphan).unwrap_err();
    assert_eq!(err.code(), "not_found", "got {err}");
}

#[test]
fn a_timed_reading_is_filed_under_the_day_its_instant_falls_on() {
    // The instant is the more precise of the two, so it decides the day.
    // Otherwise a dose taken at 00:10 lands on the calendar a day away
    // from the entry it was ticked under.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let dose = a_tracked(
        &vault,
        everyday_core::Tracker::new("Ibuprofen", everyday_core::TrackerKind::Dose),
    );

    let mut reading =
        everyday_core::Reading::on(dose.id, jiff::civil::Date::constant(2026, 3, 14), 400.0);
    reading.at = Some("2026-03-15T09:00:00Z".parse().unwrap());
    reading.tz = "UTC".into();
    vault.save_reading(&reading).unwrap();

    let stored = vault.reading(reading.id).unwrap();
    assert_eq!(stored.local_date, jiff::civil::Date::constant(2026, 3, 15));
}

#[test]
fn deleting_a_tracker_takes_its_readings_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let gone = a_tracked(
        &vault,
        everyday_core::Tracker::new("Ibuprofen", everyday_core::TrackerKind::Dose),
    );
    let kept =
        a_tracked(&vault, everyday_core::Tracker::new("Floss", everyday_core::TrackerKind::Check));

    let day = jiff::civil::Date::constant(2026, 3, 14);
    for (tracker, value) in [(gone.id, 400.0), (gone.id, 400.0), (kept.id, 1.0)] {
        vault.save_reading(&everyday_core::Reading::on(tracker, day, value)).unwrap();
    }

    assert_eq!(vault.delete_tracker(gone.id).unwrap(), 2);

    let left = vault.readings(&everyday_core::ReadingQuery::default()).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].tracker_id, kept.id);
    assert!(vault.tracker(gone.id).is_err(), "the definition goes with them");
}

#[test]
fn archiving_a_tracker_keeps_every_reading_it_ever_made() {
    // The non-destructive half of the pair, and the usual answer to "I
    // stopped taking this in March": off the page, still in the data.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut tracker = a_tracked(
        &vault,
        everyday_core::Tracker::new("Sertraline", everyday_core::TrackerKind::Dose),
    );
    vault
        .save_reading(&everyday_core::Reading::on(
            tracker.id,
            jiff::civil::Date::constant(2026, 3, 14),
            50.0,
        ))
        .unwrap();

    tracker.archived = true;
    vault.save_tracker(&tracker).unwrap();

    let journal = vault.journals().unwrap().remove(0);
    let all = vault.trackers().unwrap();
    assert!(journal.shown(&all).is_empty(), "an archived tracker leaves the page");
    assert_eq!(
        vault.readings(&everyday_core::ReadingQuery::default()).unwrap().len(),
        1,
        "and takes nothing with it"
    );
}

#[test]
fn merging_two_trackers_keeps_both_histories_and_the_chip() {
    // What tidying up a lazily created tracker has to do: `#swim` on
    // Monday and `#swimming` on Friday are one thing, and the answer
    // cannot be to throw a month of numbers away.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let scruffy =
        a_tracked(&vault, everyday_core::Tracker::new("swim", everyday_core::TrackerKind::Amount));
    let proper = a_tracked(
        &vault,
        everyday_core::Tracker::new("Swimming", everyday_core::TrackerKind::Amount),
    );
    let day = jiff::civil::Date::constant(2026, 6, 1);
    vault.save_reading(&everyday_core::Reading::on(scruffy.id, day, 30.0)).unwrap();
    vault.save_reading(&everyday_core::Reading::on(proper.id, day, 45.0)).unwrap();

    assert_eq!(vault.merge_trackers(scruffy.id, proper.id).unwrap(), 1);
    assert!(vault.tracker(scruffy.id).is_err());

    let left = vault.readings(&everyday_core::ReadingQuery::default()).unwrap();
    assert_eq!(left.len(), 2, "no reading may be lost in a merge");
    assert!(left.iter().all(|r| r.tracker_id == proper.id));

    // Every journal that drew the old chip draws the new one. Left
    // alone, the strip would silently stop showing anything.
    for journal in vault.journals().unwrap() {
        assert!(!journal.shows(scruffy.id));
        assert!(journal.shows(proper.id), "the chip must survive the merge");
    }
}

#[test]
fn a_tracker_cannot_be_merged_into_itself() {
    // It would delete the definition and leave every reading pointing at
    // it, which is exactly the unnameable-row state this domain refuses.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let t =
        a_tracked(&vault, everyday_core::Tracker::new("Steps", everyday_core::TrackerKind::Amount));
    assert_eq!(vault.merge_trackers(t.id, t.id).unwrap_err().code(), "invalid");
    assert!(vault.tracker(t.id).is_ok(), "the refusal must leave it alone");
}

#[test]
fn a_tracker_saved_with_nonsense_in_it_is_tidied_on_the_way_in() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut tracker = everyday_core::Tracker::new("  Water  ", everyday_core::TrackerKind::Amount);
    tracker.scale_max = f64::NAN;
    tracker.default_value = -4.0;
    vault.save_tracker(&tracker).unwrap();

    let stored = vault.tracker(tracker.id).unwrap();
    assert_eq!(stored.name, "Water");
    assert_eq!(stored.default_value, 1.0);
    assert!(stored.scale_max.is_finite());

    // A nameless tracker is not a tracker; it is an empty row in a form.
    let nameless = everyday_core::Tracker::new("   ", everyday_core::TrackerKind::Check);
    assert_eq!(vault.save_tracker(&nameless).unwrap_err().code(), "invalid");
}

#[test]
fn definitions_left_inside_a_journal_are_moved_once_and_then_left_alone() {
    // The one migration in the application that cannot be a SQL step:
    // the definitions are inside a sealed journal payload, and nothing
    // running against the database can read a word of it.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut journal = everyday_core::Journal::new("Health");
    let old = everyday_core::Tracker::new("Sertraline", everyday_core::TrackerKind::Dose);
    let old_id = old.id;
    journal.trackers.push(old);
    vault.save_journal(&journal).unwrap();

    // A reading written against it before the move, which must still
    // resolve afterwards.
    let day = jiff::civil::Date::constant(2026, 3, 14);
    let reading = everyday_core::Reading::on(old_id, day, 50.0);
    // Saved through the store rather than the vault, because the vault
    // refuses a reading whose tracker it cannot find -- which is the
    // state a pre-migration vault is in.
    vault.with_store(|s| s.trackers().expect("a tracking backend").put_reading(&reading)).unwrap();

    assert_eq!(vault.migrate_journal_trackers().unwrap(), 1);

    let moved = vault.tracker(old_id).expect("the definition is a record now");
    assert_eq!(moved.name, "Sertraline");
    let after = vault.journal(journal.id).unwrap();
    assert!(after.trackers.is_empty(), "the old field is emptied as it is read");
    assert!(after.shows(old_id), "and the journal keeps drawing its chip");
    assert_eq!(vault.reading(reading.id).unwrap().value, 50.0);

    // Idempotent: a second run finds nothing to do, and cannot undo the
    // edits made to what the first one moved.
    let mut edited = vault.tracker(old_id).unwrap();
    edited.name = "Sertraline 50".into();
    vault.save_tracker(&edited).unwrap();
    assert_eq!(vault.migrate_journal_trackers().unwrap(), 0);
    assert_eq!(vault.tracker(old_id).unwrap().name, "Sertraline 50");
}

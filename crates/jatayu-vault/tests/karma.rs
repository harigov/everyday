//! Karma over a real SQLite backend: the deeds the vault reads from each
//! domain, a habit being broken costing Karma, and nothing from before the
//! day Karma started counting.

mod support;
use support::{a_tracked, vault};

use jatayu_core::karma::{DeedKind, HABIT_DEFAULT, TASK_POINTS};
use jatayu_core::model::{local_date_in, system_tz};
use jatayu_core::task::{Task, TaskStatus};
use jatayu_core::{Reading, Tracker, TrackerKind};
use jiff::ToSpan;

fn done(title: &str, at: jiff::Timestamp) -> Task {
    let mut task = Task::new(title);
    task.status = TaskStatus::Done;
    task.completed_at = Some(at);
    task
}

#[test]
fn a_day_earns_for_what_was_done_and_loses_for_what_slipped() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let now = jiff::Timestamp::now();
    let today = local_date_in(now, &system_tz());
    let yesterday = today.yesterday().unwrap();

    // A report before Karma has started writes nothing.
    vault.karma(today, today, today).unwrap();
    assert!(vault.karma_book().unwrap().is_none(), "a report is a read");
    // Settling after an unlock starts it today.
    assert_eq!(vault.settle_karma(today).unwrap().since, today);

    vault.save_task(&done("Call the plumber", now)).unwrap();
    // Finished yesterday, before Karma started: worth nothing.
    vault.save_task(&done("Old chore", now - 24.hours())).unwrap();

    let walk = a_tracked(&vault, Tracker::new("Walk", TrackerKind::Check));
    vault.save_reading(&Reading::on(walk.id, today, 1.0)).unwrap();
    vault.save_reading(&Reading::on(walk.id, yesterday, 1.0)).unwrap();

    let mut drink = Tracker::new("Alcohol", TrackerKind::Check);
    drink.karma = Some(-20);
    let drink = a_tracked(&vault, drink);
    vault.save_reading(&Reading::on(drink.id, today, 1.0)).unwrap();

    let report = vault.karma(yesterday, today, today).unwrap();
    assert_eq!(report.since, today);
    assert_eq!(report.days.len(), 1, "yesterday is before Karma started: {:?}", report.days);
    let day = &report.days[0];
    assert_eq!(day.date, today);
    assert_eq!(day.earned, TASK_POINTS + HABIT_DEFAULT);
    assert_eq!(day.lost, -20);
    assert_eq!(day.net, TASK_POINTS + HABIT_DEFAULT - 20);
    assert_eq!(report.total, i64::from(day.net));
    assert_eq!(report.today, day.net);
    assert!(day.deeds.iter().any(|d| d.kind == DeedKind::Habit && d.label == "Alcohol"));
    assert!(!day.deeds.iter().any(|d| d.label == "Old chore"));
}

#[test]
fn the_start_is_kept_once_written() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let first: jiff::civil::Date = "2026-11-16".parse().unwrap();
    vault.settle_karma(first).unwrap();
    let later = vault.settle_karma("2026-12-25".parse().unwrap()).unwrap();
    assert_eq!(later.since, first, "a later settling does not move the start");
}

#[test]
fn a_settled_day_keeps_what_it_came_to() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let now = jiff::Timestamp::now();
    let today = local_date_in(now, &system_tz());
    let started = today.checked_sub(20.days()).unwrap();
    vault.settle_karma(started).unwrap();

    // A task finished ten days ago, then settled.
    let mut task = done("Fix the gate", now - (10 * 24).hours());
    vault.save_task(&task).unwrap();
    let ten_ago = local_date_in(now - (10 * 24).hours(), &system_tz());
    let book = vault.settle_karma(today).unwrap();
    assert_eq!(book.settled_through, Some(today.checked_sub(8.days()).unwrap()));
    assert_eq!(
        book.settled.iter().map(|d| (d.date, d.net)).collect::<Vec<_>>(),
        vec![(ten_ago, TASK_POINTS)]
    );

    // Reopening it afterwards does not rewrite a settled day.
    task.status = TaskStatus::Todo;
    task.completed_at = None;
    vault.save_task(&task).unwrap();
    let report = vault.karma(started, today, today).unwrap();
    assert_eq!(report.total, i64::from(TASK_POINTS));
    let day = report.days.iter().find(|d| d.date == ten_ago).expect("the settled day is drawn");
    assert_eq!(day.net, TASK_POINTS);
    assert!(day.deeds.is_empty(), "the records no longer show the deed");

    // And settling again the same day writes nothing new.
    assert_eq!(vault.settle_karma(today).unwrap(), book);
}

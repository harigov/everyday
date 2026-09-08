//! Behaviour checks for the SQLite backend.
//!
//! Two kinds. The conformance suite from `everyday_core::store` is run
//! against this backend both encrypted and not, which is what makes "the
//! rest of the app never learns which store is in use" a checked claim
//! rather than an aspiration. The rest are about *this* backend: what it
//! keeps in the clear, what it seals, and what its migrations do to a
//! database written by an earlier version.

use super::{DB_FILENAME, MEDIA_DIRNAME, SqliteStore};
use crate::schema::SCHEMA_VERSION;
use everyday_core::calendar::Event;
use everyday_core::crypto::{AeadCipher, Cipher, NullCipher, SecretKey};
use everyday_core::model::Entry;
use everyday_core::store::calendars::{CalendarStore, EventQuery};
use everyday_core::store::conformance;
use everyday_core::store::tasks::{TaskQuery, TaskSort, TaskStore};
use everyday_core::store::{EntryQuery, JournalStore, SortOrder, StoreContext};
use everyday_core::task::{Project, Task, TimeBlock};
use everyday_core::{RichDoc, model::Journal};
use std::path::Path;
use std::sync::Arc;

fn ctx(root: &Path, encrypted: bool) -> StoreContext {
    let cipher: Arc<dyn Cipher> = if encrypted {
        Arc::new(AeadCipher::new(&SecretKey::from_bytes([5u8; 32])))
    } else {
        Arc::new(NullCipher)
    };
    StoreContext { root: root.to_path_buf(), cipher }
}

#[test]
fn passes_the_shared_conformance_suite_when_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    conformance::run_all(&store);
}

#[test]
fn passes_the_shared_conformance_suite_unencrypted() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), false)).unwrap();
    conformance::run_all(&store);
}

#[test]
fn data_survives_closing_and_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::new("Daily");
    let mut e = Entry::new(j.id, "UTC");
    e.body = RichDoc::from_plain_text("written before the restart");

    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_journal(&j).unwrap();
        store.put_entry(&e).unwrap();
        store.flush().unwrap();
    }

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert_eq!(store.get_journal(j.id).unwrap().name, "Daily");
    assert_eq!(store.get_entry(e.id).unwrap().body.plain_text(), "written before the restart");
}

#[test]
fn the_database_file_contains_no_readable_entry_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let j = Journal::new("A journal name nobody should see");
    store.put_journal(&j).unwrap();
    let mut e = Entry::new(j.id, "UTC");
    e.title = "A title nobody should see".into();
    e.body = RichDoc::from_plain_text("body text nobody should see");
    e.tags = vec!["secrettag".into()];
    store.put_entry(&e).unwrap();
    store.flush().unwrap();

    let raw = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"A journal name nobody should see".as_slice(),
        b"A title nobody should see",
        b"body text nobody should see",
        b"secrettag",
    ] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn a_database_written_under_one_key_does_not_open_under_another() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::new("Private");
    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_journal(&j).unwrap();
    }
    let wrong = StoreContext {
        root: dir.path().to_path_buf(),
        cipher: Arc::new(AeadCipher::new(&SecretKey::from_bytes([6u8; 32]))),
    };
    let store = SqliteStore::open(wrong).unwrap();
    assert_eq!(store.get_journal(j.id).unwrap_err().code(), "decrypt_failed");
}

#[test]
fn pagination_pushed_into_sql_matches_the_in_memory_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let j = Journal::new("Long");
    store.put_journal(&j).unwrap();

    for d in 1..=20u8 {
        let mut e = Entry::new(j.id, "UTC");
        e.local_date = jiff::civil::date(2025, 1, d as i8);
        e.title = format!("day {d:02}");
        e.tags = vec!["daily".into()];
        store.put_entry(&e).unwrap();
    }

    // No tags -> SQL LIMIT/OFFSET. With tags -> in-memory pass. Both must
    // return the same window.
    let sql_path =
        EntryQuery { sort: SortOrder::DateAsc, offset: 5, limit: Some(4), ..Default::default() };
    let memory_path = EntryQuery { tags: vec!["daily".into()], ..sql_path.clone() };

    let a: Vec<String> =
        store.list_entries(&sql_path).unwrap().into_iter().map(|r| r.title).collect();
    let b: Vec<String> =
        store.list_entries(&memory_path).unwrap().into_iter().map(|r| r.title).collect();

    assert_eq!(a, ["day 06", "day 07", "day 08", "day 09"]);
    assert_eq!(a, b, "SQL and in-memory paths must agree");
}

#[test]
fn title_sort_falls_back_to_the_in_memory_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let j = Journal::new("Sorted");
    store.put_journal(&j).unwrap();

    for (d, title) in [(1u8, "zebra"), (2, "apple"), (3, "Mango")] {
        let mut e = Entry::new(j.id, "UTC");
        e.local_date = jiff::civil::date(2025, 2, d as i8);
        e.title = title.into();
        store.put_entry(&e).unwrap();
    }
    let q = EntryQuery { sort: SortOrder::TitleAsc, ..Default::default() };
    let titles: Vec<String> =
        store.list_entries(&q).unwrap().into_iter().map(|r| r.title).collect();
    assert_eq!(titles, ["apple", "Mango", "zebra"], "title sort must ignore case");
}

#[test]
fn an_unlimited_query_returns_everything_despite_the_offset_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let j = Journal::new("All");
    store.put_journal(&j).unwrap();
    for d in 1..=5u8 {
        let mut e = Entry::new(j.id, "UTC");
        e.local_date = jiff::civil::date(2025, 3, d as i8);
        store.put_entry(&e).unwrap();
    }
    assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 5);
    let offset_only = EntryQuery { offset: 2, ..Default::default() };
    assert_eq!(store.list_entries(&offset_only).unwrap().len(), 3);
}

#[test]
fn media_lives_outside_the_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let id = store.put_blob(&vec![7u8; 300_000]).unwrap();

    assert!(dir.path().join(MEDIA_DIRNAME).is_dir());
    let db_len = std::fs::metadata(dir.path().join(DB_FILENAME)).unwrap().len();
    assert!(db_len < 200_000, "a 300 KB attachment must not land in the database");
    assert_eq!(store.get_blob(id).unwrap().len(), 300_000);
}

#[test]
fn the_database_file_contains_no_readable_task_text() {
    // The task tables make the same promise the entry table does: the
    // shape of the work is in the clear, never its contents.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let mut p = Project::new("A project nobody should see");
    p.notes = "project notes nobody should see".into();
    p.tags = vec!["secretprojecttag".into()];
    store.put_project(&p).unwrap();

    let mut t = Task::new("A task nobody should see").in_project(p.id);
    t.notes = "task notes nobody should see".into();
    t.tags = vec!["secrettasktag".into()];
    store.put_task(&t).unwrap();

    let start = "2026-06-15T09:00:00Z".parse::<jiff::Timestamp>().unwrap();
    let mut b =
        TimeBlock::new(everyday_core::task::BlockSubject::Task { id: t.id }, start, 60, "UTC");
    b.title = "A block nobody should see".into();
    b.notes = "block notes nobody should see".into();
    store.put_block(&b).unwrap();
    store.flush().unwrap();

    let raw = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"A project nobody should see".as_slice(),
        b"project notes nobody should see",
        b"secretprojecttag",
        b"A task nobody should see",
        b"task notes nobody should see",
        b"secrettasktag",
        b"A block nobody should see",
        b"block notes nobody should see",
    ] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn task_filters_pushed_into_sql_match_the_in_memory_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let p = Project::new("Agreement");
    store.put_project(&p).unwrap();

    for d in 1..=20u8 {
        let mut t = Task::new(format!("task {d:02}")).in_project(p.id);
        t.due_date = Some(jiff::civil::date(2026, 1, d as i8));
        t.sort_order = i32::from(d);
        t.tags = vec!["everything".into()];
        store.put_task(&t).unwrap();
    }

    // No tags and no text -> SQL LIMIT/OFFSET and SQL ordering. With a
    // tag -> the in-memory pass. Both must return the same window.
    let sql_path = TaskQuery {
        sort: TaskSort::DueAsc,
        offset: 5,
        limit: Some(4),
        ..TaskQuery::in_project(p.id)
    };
    let memory_path = TaskQuery { tags: vec!["everything".into()], ..sql_path.clone() };

    let titles = |q: &TaskQuery| -> Vec<String> {
        store.list_tasks(q).unwrap().into_iter().map(|t| t.title).collect()
    };
    let a = titles(&sql_path);
    assert_eq!(a, ["task 06", "task 07", "task 08", "task 09"]);
    assert_eq!(a, titles(&memory_path), "SQL and in-memory paths must agree");
}

#[test]
fn undated_tasks_sort_last_on_both_paths() {
    // SQLite sorts NULL first on an ASC column, so without the explicit
    // `due_date IS NULL` term the backlog would bury what is due.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let mut soon = Task::new("soon");
    soon.due_date = Some(jiff::civil::date(2026, 3, 1));
    let undated = Task::new("someday");
    store.put_tasks(&[soon, undated]).unwrap();

    let by_due = TaskQuery { sort: TaskSort::DueAsc, ..Default::default() };
    let titles: Vec<String> =
        store.list_tasks(&by_due).unwrap().into_iter().map(|t| t.title).collect();
    assert_eq!(titles, ["soon", "someday"]);

    // And with a tag filter, which routes through `TaskQuery::apply`.
    let via_memory = TaskQuery { text: "s".into(), ..by_due };
    let titles: Vec<String> =
        store.list_tasks(&via_memory).unwrap().into_iter().map(|t| t.title).collect();
    assert_eq!(titles, ["soon", "someday"], "both paths must agree on undated tasks");
}

#[test]
fn a_version_1_database_gains_the_task_tables_without_losing_entries() {
    // The migration people will actually run: a vault written before the
    // todo app existed, opened by a build that has it.
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::new("Written under v1");
    let mut e = Entry::new(j.id, "UTC");
    e.body = RichDoc::from_plain_text("this must survive the migration");

    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_journal(&j).unwrap();
        store.put_entry(&e).unwrap();
        // Rewind to the world as version 1 left it: the task tables gone
        // and the recorded version behind.
        let conn = store.conn.lock().unwrap();
        conn.execute_batch("DROP TABLE tasks; DROP TABLE projects; DROP TABLE time_blocks;")
            .unwrap();
        conn.pragma_update(None, "user_version", 1i64).unwrap();
    }

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert_eq!(
        store.get_entry(e.id).unwrap().body.plain_text(),
        "this must survive the migration",
        "migrating must not disturb what was already there"
    );
    assert_eq!(store.get_journal(j.id).unwrap().name, "Written under v1");

    let t = Task::new("and the new tables must work");
    store.put_task(&t).unwrap();
    assert_eq!(store.get_task(t.id).unwrap(), t);
}

#[test]
fn a_version_2_database_gains_the_calendar_tables_without_losing_tasks() {
    // The next migration people will actually run: a vault written
    // before the calendar existed, opened by a build that has it.
    let dir = tempfile::tempdir().unwrap();
    let t = Task::new("this must survive the migration");

    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_task(&t).unwrap();
        let conn = store.conn.lock().unwrap();
        conn.execute_batch("DROP TABLE events; DROP TABLE calendars;").unwrap();
        conn.pragma_update(None, "user_version", 2i64).unwrap();
    }

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert_eq!(store.get_task(t.id).unwrap(), t, "migrating must not disturb the todo app");

    let cal = everyday_core::calendar::Calendar::subscribed(
        "and the new tables must work",
        "https://example.com/x.ics",
    );
    store.put_calendar(&cal).unwrap();
    assert_eq!(store.get_calendar(cal.id).unwrap(), cal);
}

/// A minimal event for the tests below; the shared suite covers the rest.
fn an_event(calendar_id: everyday_core::CalendarId, title: &str, location: &str) -> Event {
    Event {
        id: everyday_core::EventId::new(),
        calendar_id,
        uid: "uid-1".into(),
        title: title.into(),
        description: String::new(),
        location: location.into(),
        start: "2026-06-15T09:00:00Z".parse().unwrap(),
        end: "2026-06-15T10:00:00Z".parse().unwrap(),
        local_date: jiff::civil::date(2026, 6, 15),
        end_date: jiff::civil::date(2026, 6, 15),
        tz: "UTC".into(),
        all_day: false,
        status: everyday_core::EventStatus::Confirmed,
        organizer: String::new(),
        url: String::new(),
        busy: true,
        updated_at: jiff::Timestamp::now(),
    }
}

#[test]
fn the_database_file_contains_no_readable_feed_url() {
    // A subscription URL is a bearer credential: anyone holding one can
    // read that calendar until it is revoked. It must never sit in a
    // clear column beside the dates it indexes.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let cal = everyday_core::calendar::Calendar::subscribed(
        "A calendar nobody should see",
        "https://calendar.example.com/private/secretfeedtoken/basic.ics",
    );
    store.put_calendar(&cal).unwrap();

    let mut event = an_event(cal.id, "A meeting nobody should see", "A room nobody should see");
    event.description = "meeting notes nobody should see".into();
    store.replace_events(cal.id, std::slice::from_ref(&event)).unwrap();
    store.flush().unwrap();

    let raw = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"secretfeedtoken".as_slice(),
        b"calendar.example.com",
        b"A calendar nobody should see",
        b"A meeting nobody should see",
        b"A room nobody should see",
        b"meeting notes nobody should see",
    ] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn the_event_text_filter_agrees_with_the_pushed_down_window() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let cal = everyday_core::calendar::Calendar::subscribed("Work", "https://example.com/work.ics");
    store.put_calendar(&cal).unwrap();

    let events: Vec<Event> = (1..=10u8)
        .map(|d| {
            let mut e = an_event(cal.id, &format!("standup {d:02}"), "Room 4");
            e.uid = format!("uid-{d}");
            e.local_date = jiff::civil::date(2026, 6, d as i8);
            e.end_date = e.local_date;
            e.start = format!("2026-06-{d:02}T09:00:00Z").parse().unwrap();
            e.end = format!("2026-06-{d:02}T09:15:00Z").parse().unwrap();
            e
        })
        .collect();
    store.replace_events(cal.id, &events).unwrap();

    // The window is SQL; the text filter is the in-memory pass. Applied
    // together they must narrow the same set rather than fight.
    let window = EventQuery::between(jiff::civil::date(2026, 6, 3), jiff::civil::date(2026, 6, 7));
    assert_eq!(store.list_events(&window).unwrap().len(), 5);

    let with_text = EventQuery { text: "standup 05".into(), ..window.clone() };
    let hits = store.list_events(&with_text).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "standup 05");

    // A text match outside the window is still outside it.
    let elsewhere = EventQuery { text: "standup 09".into(), ..window };
    assert!(store.list_events(&elsewhere).unwrap().is_empty());
}

#[test]
fn hidden_calendars_are_excluded_by_the_visible_only_query() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let shown = everyday_core::calendar::Calendar::subscribed("Shown", "https://example.com/a.ics");
    let mut hidden =
        everyday_core::calendar::Calendar::subscribed("Hidden", "https://example.com/b.ics");
    hidden.visible = false;
    store.put_calendar(&shown).unwrap();
    store.put_calendar(&hidden).unwrap();
    store.replace_events(shown.id, &[an_event(shown.id, "shown", "")]).unwrap();
    store.replace_events(hidden.id, &[an_event(hidden.id, "hidden", "")]).unwrap();

    let all = store.list_events(&EventQuery::default()).unwrap();
    assert_eq!(all.len(), 2, "hiding is a view setting, not a deletion");

    let visible =
        store.list_events(&EventQuery { visible_only: true, ..Default::default() }).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].title, "shown");
}

#[test]
fn reopening_does_not_re_run_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        let conn = store.conn.lock().unwrap();
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}

#[test]
fn a_database_from_a_newer_build_is_refused_rather_than_written_to() {
    // The store used to return `Ok` for any version at or above its own,
    // which meant an older build opened a newer vault, skipped every
    // migration step and wrote into a schema it did not understand.
    let dir = tempfile::tempdir().unwrap();
    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        let conn = store.conn.lock().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
    }
    let Err(err) = SqliteStore::open(ctx(dir.path(), true)) else {
        panic!("a newer schema version must be refused");
    };
    assert_eq!(err.code(), "unsupported_version");
}

#[test]
fn a_feed_repeating_an_event_id_still_syncs() {
    // A publisher reusing a UID broke the primary key, which aborted the
    // whole transaction -- so that calendar could never sync again, not once.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let cal =
        everyday_core::calendar::Calendar::subscribed("Fixtures", "https://example.invalid/f.ics");
    store.put_calendar(&cal).unwrap();

    let mut first = an_event(cal.id, "kickoff", "the ground");
    let mut duplicate = an_event(cal.id, "kickoff (again)", "the ground");
    duplicate.id = first.id;
    first.uid = "same".into();
    duplicate.uid = "same".into();

    store.replace_events(cal.id, &[first, duplicate]).unwrap();

    let events = store.list_events(&EventQuery::default()).unwrap();
    assert_eq!(events.len(), 1, "the repeat collapses onto the first");
    assert_eq!(events[0].title, "kickoff (again)", "last one wins");
}

#[test]
fn an_event_filed_into_another_calendar_agrees_with_itself() {
    // The clear column was corrected to the calendar being synced while the
    // sealed payload kept the original, so the event that came back named a
    // feed it was not stored under.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let mine =
        everyday_core::calendar::Calendar::subscribed("Mine", "https://example.invalid/a.ics");
    let theirs =
        everyday_core::calendar::Calendar::subscribed("Theirs", "https://example.invalid/b.ics");
    store.put_calendar(&mine).unwrap();
    store.put_calendar(&theirs).unwrap();

    // An event claiming to belong to `theirs`, synced as part of `mine`.
    let stray = an_event(theirs.id, "misfiled", "elsewhere");
    store.replace_events(mine.id, std::slice::from_ref(&stray)).unwrap();

    let got = store.get_event(stray.id).unwrap();
    assert_eq!(got.calendar_id, mine.id, "the payload must name where it was stored");

    // And it is reachable by the calendar it was filed under.
    let by_calendar = store
        .list_events(&EventQuery { calendar_id: Some(mine.id), ..Default::default() })
        .unwrap();
    assert_eq!(by_calendar.len(), 1);
}

#[test]
fn quick_check_passes_on_a_healthy_database() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let j = Journal::new("Daily");
    store.put_journal(&j).unwrap();
    store.put_entry(&Entry::new(j.id, "UTC")).unwrap();

    assert!(store.check_integrity().unwrap().is_empty());
}

#[test]
fn a_snapshot_is_a_database_that_opens_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let j = Journal::new("Daily");
    store.put_journal(&j).unwrap();
    let mut e = Entry::new(j.id, "UTC");
    e.title = "in the snapshot".into();
    store.put_entry(&e).unwrap();
    let blob = store.put_blob(b"an attachment").unwrap();

    let dest = tempfile::tempdir().unwrap();
    let into = dest.path().join("copy");
    store.snapshot(&into).unwrap();

    // Opened as a store in its own right, under the same key.
    let copy = SqliteStore::open(ctx(&into, true)).unwrap();
    assert_eq!(copy.get_entry(e.id).unwrap().title, "in the snapshot");
    assert_eq!(copy.get_blob(blob).unwrap(), b"an attachment");
    assert!(copy.check_integrity().unwrap().is_empty());

    // A snapshot, not a live view.
    let mut later = Entry::new(j.id, "UTC");
    later.title = "after".into();
    store.put_entry(&later).unwrap();
    assert!(copy.get_entry(later.id).is_err());
}

#[test]
fn a_snapshot_refuses_to_land_on_an_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let dest = tempfile::tempdir().unwrap();
    store.snapshot(dest.path()).unwrap();
    let err = store.snapshot(dest.path()).expect_err("a second snapshot must refuse");
    assert_eq!(err.code(), "invalid");
}

#[test]
fn garbage_collection_spares_a_freshly_stored_attachment() {
    // The image pasted into a draft that has not been saved yet: it is
    // unreferenced by every measure GC has, and deleting it loses it.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let pasted = store.put_blob(b"pixels").unwrap();

    assert_eq!(store.collect_garbage(std::time::Duration::from_secs(60)).unwrap(), 0);
    assert!(store.has_blob(pasted).unwrap());

    // With no grace it is collectable, which is what `gc --include-recent` is.
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 1);
}

#[test]
fn a_second_handle_sees_the_first_ones_writes() {
    // The premise the vault's write lock and the conditional put both rest
    // on: two handles on one SQLite file are looking at the same data, so
    // "someone else changed it since you loaded it" is a real thing that can
    // happen rather than a hypothetical.
    let dir = tempfile::tempdir().unwrap();
    let first = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let second = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let j = Journal::new("Shared");
    first.put_journal(&j).unwrap();
    let mut e = Entry::new(j.id, "UTC");
    e.title = "written by the first".into();
    first.put_entry(&e).unwrap();

    assert_eq!(second.get_entry(e.id).unwrap().title, "written by the first");

    // And the conditional put across the two: the second handle saves, so
    // the first one's version token goes stale.
    let loaded = second.get_entry(e.id).unwrap().updated_at;
    let mut theirs = e.clone();
    theirs.title = "written by the second".into();
    theirs.updated_at = jiff::Timestamp::now();
    second.put_entry_if(&theirs, Some(loaded)).unwrap();

    let mut mine = e.clone();
    mine.title = "and then by the first".into();
    mine.updated_at = jiff::Timestamp::now();
    assert_eq!(
        first.put_entry_if(&mine, Some(loaded)).unwrap_err().code(),
        "conflict",
        "the stale writer must be refused, not allowed to clobber"
    );
    assert_eq!(first.get_entry(e.id).unwrap().title, "written by the second");
}

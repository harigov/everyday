//! Behaviour checks for the SQLite backend.
//!
//! Two kinds. The conformance suite from `karmayoga_core::store` is run
//! against this backend both encrypted and not, which is what makes "the
//! rest of the app never learns which store is in use" a checked claim
//! rather than an aspiration. The rest are about *this* backend: what it
//! keeps in the clear, what it seals, and what its migrations do to a
//! database written by an earlier version.

use super::{DB_FILENAME, MEDIA_DIRNAME, SqliteFactory, SqliteStore};
use karmayoga_core::calendar::Event;
use karmayoga_core::crypto::{AeadCipher, Cipher, NullCipher, SecretKey};
use karmayoga_core::id::{AccountId, MailMessageId, TemplateId, ThreadId, TranscriptId};
use karmayoga_core::mail::{Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags};
use karmayoga_core::meeting::{
    Attribution, Recording, Segment, Speaker, Stage, Transcript, Voiceprint,
};
use karmayoga_core::model::Entry;
use karmayoga_core::note::Note;
use karmayoga_core::packstore::{PackRef, PackStore, run_pack_store_suite};
use karmayoga_core::search::SearchScope;
use karmayoga_core::store::BackendRegistry;
use karmayoga_core::store::calendars::{CalendarStore, EventQuery};
use karmayoga_core::store::conformance;
use karmayoga_core::store::mail::{IngestMessage, MailStore, ThreadFilter};
use karmayoga_core::store::meetings::MeetingStore;
use karmayoga_core::store::notes::NoteStore;
use karmayoga_core::store::purpose::PurposeStore;
use karmayoga_core::store::secrets::SecretStore;
use karmayoga_core::store::tasks::{TaskQuery, TaskSort, TaskStore};
use karmayoga_core::store::trackers::{ReadingQuery, TrackerStore};
use karmayoga_core::store::{EntryQuery, JournalStore, SortOrder, StoreContext};
use karmayoga_core::task::{Project, Task, TimeBlock};
use karmayoga_core::tracker::{Aggregate, Reading, Tracker, TrackerKind};
use karmayoga_core::{NoteId, Vault, VaultConfig, VoiceprintId};
use karmayoga_core::{PackId, RichDoc, model::Journal};
use karmayoga_store_sql::packs::TablePacks;
use karmayoga_store_sql::schema::SCHEMA_VERSION;
use std::path::Path;
use std::sync::Arc;

fn ctx(root: &Path, encrypted: bool) -> StoreContext {
    let cipher: Arc<dyn Cipher> = if encrypted {
        Arc::new(AeadCipher::new(&SecretKey::from_bytes([5u8; 32])))
    } else {
        Arc::new(NullCipher)
    };
    StoreContext::new(root, cipher)
}

/// A plain connection to the database file, for the handful of tests that
/// have to tamper with it or read a pragma off it.
///
/// Deliberately not a way in through the store's own connection. These tests
/// are about what is *in the file* -- an older schema, a version pragma, a
/// column that must not hold readable text -- and reaching them through the
/// store would let a change to the store's internals quietly change what
/// they check. The store should be closed first, so the WAL is behind us.
fn raw(dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join(DB_FILENAME)).unwrap()
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
    let wrong =
        StoreContext::new(dir.path(), Arc::new(AeadCipher::new(&SecretKey::from_bytes([6u8; 32]))));
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
        TimeBlock::new(karmayoga_core::task::BlockSubject::Task { id: t.id }, start, 60, "UTC");
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
    }
    // Rewind to the world as version 1 left it: the task tables gone and the
    // recorded version behind.
    {
        let conn = raw(dir.path());
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
    }
    {
        let conn = raw(dir.path());
        conn.execute_batch("DROP TABLE events; DROP TABLE calendars;").unwrap();
        conn.pragma_update(None, "user_version", 2i64).unwrap();
    }

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert_eq!(store.get_task(t.id).unwrap(), t, "migrating must not disturb the todo app");

    let cal = karmayoga_core::calendar::Calendar::subscribed(
        "and the new tables must work",
        "https://example.com/x.ics",
    );
    store.put_calendar(&cal).unwrap();
    assert_eq!(store.get_calendar(cal.id).unwrap(), cal);
}

/// A minimal event for the tests below; the shared suite covers the rest.
fn an_event(calendar_id: karmayoga_core::CalendarId, title: &str, location: &str) -> Event {
    Event {
        id: karmayoga_core::EventId::new(),
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
        status: karmayoga_core::EventStatus::Confirmed,
        organizer: String::new(),
        attendees: Vec::new(),
        guests: Vec::new(),
        url: String::new(),
        busy: true,
        series: None,
        own: None,
        alerts: None,
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

    let cal = karmayoga_core::calendar::Calendar::subscribed(
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
    let cal =
        karmayoga_core::calendar::Calendar::subscribed("Work", "https://example.com/work.ics");
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

    let shown =
        karmayoga_core::calendar::Calendar::subscribed("Shown", "https://example.com/a.ics");
    let mut hidden =
        karmayoga_core::calendar::Calendar::subscribed("Hidden", "https://example.com/b.ics");
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
        drop(SqliteStore::open(ctx(dir.path(), true)).unwrap());
        let v = raw(dir.path())
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}

#[test]
fn a_database_from_a_newer_build_is_refused_rather_than_written_to() {
    // The store used to return `Ok` for any version at or above its own,
    // which meant an older build opened a newer vault, skipped every
    // migration step and wrote into a schema it did not understand.
    let dir = tempfile::tempdir().unwrap();
    drop(SqliteStore::open(ctx(dir.path(), true)).unwrap());
    raw(dir.path()).pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
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
        karmayoga_core::calendar::Calendar::subscribed("Fixtures", "https://example.invalid/f.ics");
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
        karmayoga_core::calendar::Calendar::subscribed("Mine", "https://example.invalid/a.ics");
    let theirs =
        karmayoga_core::calendar::Calendar::subscribed("Theirs", "https://example.invalid/b.ics");
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
fn a_background_integrity_check_answers_like_the_inline_one_and_stops_when_cancelled() {
    use std::sync::atomic::AtomicBool;

    let dir = tempfile::tempdir().unwrap();
    drop(SqliteStore::open(ctx(dir.path(), true)).unwrap());
    // Enough pages that a cancelled scan has somewhere to stop: the cancel
    // flag is read every hundred of them, so a near-empty database would
    // finish before ever looking.
    raw(dir.path())
        .execute_batch(
            "CREATE TABLE filler (b BLOB);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
             INSERT INTO filler SELECT randomblob(1000) FROM n;",
        )
        .unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let check = |cancelled: bool| {
        let job = store
            .background_integrity_check(Arc::new(AtomicBool::new(cancelled)))
            .unwrap()
            .expect("a SQLite store has a check to run");
        // On a thread of its own, the way a vault runs it.
        std::thread::spawn(job).join().unwrap()
    };
    assert!(check(false).unwrap().is_empty());
    assert!(check(true).is_err(), "a cancelled check must stop, not answer");
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

/// A chat link's store beside the vault's: a database of its own, in WAL
/// mode, with a write still in its log -- the shape matrix-sdk leaves.
fn chat_store(root: &Path) -> std::path::PathBuf {
    let dir = root.join("chat").join("link");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("matrix-sdk-crypto.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch("CREATE TABLE keys (k TEXT); INSERT INTO keys VALUES ('device key');")
        .unwrap();
    // Left open, so the row is still in the -wal when the copy is made.
    std::mem::forget(conn);
    path
}

#[test]
fn a_snapshot_copies_mail_packs_and_a_chat_store_not_only_media() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    std::fs::create_dir_all(dir.path().join("mail-packs/acct")).unwrap();
    std::fs::write(dir.path().join("mail-packs/acct/0001.pack"), b"sealed frames").unwrap();
    chat_store(dir.path());

    let dest = tempfile::tempdir().unwrap();
    let into = dest.path().join("copy");
    store.snapshot(&into).unwrap();
    assert_eq!(std::fs::read(into.join("mail-packs/acct/0001.pack")).unwrap(), b"sealed frames");
    let copied =
        rusqlite::Connection::open(into.join("chat/link/matrix-sdk-crypto.sqlite3")).unwrap();
    let k: String = copied.query_row("SELECT k FROM keys", [], |r| r.get(0)).unwrap();
    assert_eq!(k, "device key", "the write still in the log came across");
    assert!(!into.join("chat/link/matrix-sdk-crypto.sqlite3-wal").exists());
}

#[test]
fn a_backup_job_stages_each_database_and_lists_everything_else_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let j = Journal::new("Daily");
    store.put_journal(&j).unwrap();
    let blob = store.put_blob(b"an attachment").unwrap();
    std::fs::create_dir_all(dir.path().join("mail-packs/acct")).unwrap();
    std::fs::write(dir.path().join("mail-packs/acct/0001.pack"), b"frames").unwrap();
    chat_store(dir.path());

    let staging = tempfile::tempdir().unwrap();
    let job = store.backup_job().unwrap();
    let files = std::thread::spawn({
        let staging = staging.path().to_path_buf();
        move || job(&staging)
    })
    .join()
    .unwrap()
    .unwrap();
    let by_path =
        |p: &str| files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} listed"));

    let db = by_path("store/karmayoga.db");
    assert!(db.database && db.source.starts_with(staging.path()), "copied, not read live");
    let copy = rusqlite::Connection::open(&db.source).unwrap();
    let journals: i64 = copy.query_row("SELECT COUNT(*) FROM journals", [], |r| r.get(0)).unwrap();
    assert_eq!(journals, 1);
    assert!(!db.source.with_extension("db-wal").exists(), "one file, no log");

    let chat = by_path("store/chat/link/matrix-sdk-crypto.sqlite3");
    assert!(chat.database && chat.source.starts_with(staging.path()));
    let pack = by_path("store/mail-packs/acct/0001.pack");
    assert!(!pack.database && pack.source.starts_with(dir.path()), "read in place");
    assert!(pack.modified_ns.is_some());
    let hex = blob.to_hex();
    assert!(files.iter().any(|f| f.path.starts_with("store/media/") && f.path.contains(&hex)));
    assert!(
        !files.iter().any(|f| f.path.ends_with("-wal") || f.path.ends_with("-shm")),
        "no log is listed: {:?}",
        files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
}

#[test]
fn backup_settings_and_status_are_written_apart() {
    use karmayoga_core::backup::{BackupSettings, BackupStatus, Destination};
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert!(store.backup_settings().unwrap().is_none());
    assert!(store.backup_status().unwrap().is_none());

    // Status first: a row with no settings in it yet still reads as none.
    let status = BackupStatus { last_error: Some("unplugged".into()), ..BackupStatus::default() };
    store.put_backup_status(&status).unwrap();
    assert!(store.backup_settings().unwrap().is_none());

    let settings = BackupSettings {
        enabled: true,
        destination: Some(Destination::Folder { path: "/mnt/backup".into() }),
        ..BackupSettings::default()
    };
    store.put_backup_settings(&settings).unwrap();
    assert_eq!(store.backup_status().unwrap(), Some(status), "a settings save kept the status");
    let later = BackupStatus::default();
    store.put_backup_status(&later).unwrap();
    assert_eq!(
        store.backup_settings().unwrap(),
        Some(settings),
        "a status write kept the settings"
    );
}

#[test]
fn the_karma_book_is_kept_and_replaced() {
    use karmayoga_core::karma::KarmaBook;
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert!(store.karma_book().unwrap().is_none());
    let book = KarmaBook::new("2026-11-16".parse().unwrap());
    store.put_karma_book(&book).unwrap();
    assert_eq!(store.karma_book().unwrap(), Some(book));
    let later = KarmaBook::new("2026-12-01".parse().unwrap());
    store.put_karma_book(&later).unwrap();
    assert_eq!(store.karma_book().unwrap(), Some(later));
}

#[test]
fn connection_settings_and_states_are_written_apart() {
    use karmayoga_core::connection::{
        AuthMethod, Connection, ConnectionSettings, ConnectionState, ConnectionStates, Transport,
    };
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert!(store.connection_settings().unwrap().is_none());
    assert!(store.connection_states().unwrap().is_none());

    let linear = Connection::new(
        "Linear",
        Transport::Http { url: "https://mcp.linear.app/mcp".into(), headers: vec![] },
        AuthMethod::OAuth { client_id: None, scopes: vec![], redirect_port: None },
    );
    let mut states = ConnectionStates::default();
    states.states.insert(
        linear.id,
        ConnectionState { last_error: Some("unreachable".into()), ..Default::default() },
    );
    // States first: a row with no settings in it yet still reads as none.
    store.put_connection_states(&states).unwrap();
    assert!(store.connection_settings().unwrap().is_none());

    let settings = ConnectionSettings { connections: vec![linear] };
    store.put_connection_settings(&settings).unwrap();
    assert_eq!(store.connection_states().unwrap(), Some(states), "a settings save kept the states");
    store.put_connection_states(&ConnectionStates::default()).unwrap();
    assert_eq!(
        store.connection_settings().unwrap(),
        Some(settings),
        "a states write kept the settings"
    );
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

// ---- the tracking domain -------------------------------------------------

fn a_day(n: i8) -> jiff::civil::Date {
    jiff::civil::Date::constant(2026, 3, n)
}

#[test]
fn the_database_file_contains_no_readable_tracker_name() {
    // The trade this domain makes, checked. Values, dates and instants are
    // in the clear so a year of them can be aggregated without decryption --
    // and the *name* that would say what a value means is not, because it
    // lives in the journal record and is sealed with it.
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::new("Health");
    let tracker = Tracker::new("Sertraline", TrackerKind::Dose).with_unit("mg");
    let tracker_id = tracker.id;
    journal.show(tracker_id);

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    store.put_journal(&journal).unwrap();
    store.put_tracker(&tracker).unwrap();

    let mut reading = Reading::on(tracker_id, a_day(14), 50.0).in_journal(journal.id);
    reading.note = "with breakfast".into();
    store.put_reading(&reading).unwrap();
    store.flush().unwrap();

    let bytes = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(!raw.contains("Sertraline"), "a tracker name reached the database file");
    assert!(!raw.contains("with breakfast"), "a reading's note reached the database file");
    // What is deliberately visible: the day and the id it belongs to.
    assert!(raw.contains("2026-03-14"), "the date column is meant to be readable");
}

#[test]
fn readings_are_filtered_and_aggregated_by_the_same_window() {
    // `list_readings` and `tracker_days` share one `WHERE` clause precisely
    // so these two can never disagree; this is the check that they do not.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let journal = Journal::new("Health");
    let (pain, dose) = (karmayoga_core::TrackerId::new(), karmayoga_core::TrackerId::new());

    for (tracker, day, value) in
        [(pain, 1, 3.0), (pain, 1, 7.0), (pain, 2, 4.0), (dose, 1, 400.0), (dose, 5, 400.0)]
    {
        store.put_reading(&Reading::on(tracker, a_day(day), value).in_journal(journal.id)).unwrap();
    }

    let window = ReadingQuery {
        tracker_ids: vec![pain],
        from: Some(a_day(1)),
        to: Some(a_day(2)),
        ..Default::default()
    };
    assert_eq!(store.list_readings(&window).unwrap().len(), 3);

    let days = store.tracker_days(&window).unwrap();
    assert_eq!(days.len(), 2, "two days of one tracker");
    // The morning's 3 and the evening's 7 average 5. They do not add to 10.
    assert_eq!(days[0].value_for(Aggregate::Mean), 5.0);
    assert_eq!(days[0].count, 2);
    assert_eq!(days[1].value_for(Aggregate::Mean), 4.0);

    // And the SQL aggregate agrees with the arithmetic done in memory.
    assert_eq!(days, window.totals(store.list_readings(&window).unwrap()));
}

#[test]
fn a_capped_query_caps_the_list_and_not_the_aggregate() {
    // The SQL path and the in-memory one have to cover the same rows, and a
    // limit is the filter they cannot both honour. Both ignore it here; the
    // check is that they ignore it identically.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let journal = Journal::new("Health");
    let tracker = karmayoga_core::TrackerId::new();
    for value in [400.0, 400.0, 400.0] {
        store.put_reading(&Reading::on(tracker, a_day(1), value).in_journal(journal.id)).unwrap();
    }

    let capped = ReadingQuery { limit: Some(1), ..Default::default() };
    assert_eq!(store.list_readings(&capped).unwrap().len(), 1, "the list is capped");

    let days = store.tracker_days(&capped).unwrap();
    assert_eq!(days[0].count, 3, "the aggregate covers the window");
    assert_eq!(days[0].sum, 1200.0);
    // And the two implementations of it still agree.
    assert_eq!(days, capped.totals(store.list_readings(&ReadingQuery::default()).unwrap()));
}

#[test]
fn a_reading_that_never_knew_its_minute_says_so_after_a_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let journal = Journal::new("Health");
    let tracker = karmayoga_core::TrackerId::new();

    let undated = Reading::on(tracker, a_day(1), 1.0).in_journal(journal.id);
    let at: jiff::Timestamp = "2026-03-01T07:30:00Z".parse().unwrap();
    let timed = Reading::at(tracker, at, "UTC", 45.0).in_journal(journal.id);
    store.put_reading(&undated).unwrap();
    store.put_reading(&timed).unwrap();

    let all = store.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(all.len(), 2);
    // Untimed first within the day: "sometime today" is not lunchtime.
    assert_eq!(all[0].at, None);
    assert_eq!(all[1].at, Some(at));

    let timed_only = ReadingQuery { timed_only: true, ..Default::default() };
    let kept = store.list_readings(&timed_only).unwrap();
    assert_eq!(kept.len(), 1, "an unknown minute must not answer an hour-of-day query");
    assert_eq!(kept[0].id, timed.id);
    // And the aggregate over the same window excludes it too.
    assert_eq!(store.tracker_days(&timed_only).unwrap()[0].count, 1);
}

#[test]
fn deleting_a_journal_leaves_its_readings_and_only_drops_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let mine = Journal::new("Health");
    let other = Journal::new("Work");
    store.put_journal(&mine).unwrap();
    store.put_journal(&other).unwrap();

    let t = karmayoga_core::TrackerId::new();
    store.put_reading(&Reading::on(t, a_day(1), 1.0).in_journal(mine.id)).unwrap();
    store.put_reading(&Reading::on(t, a_day(1), 1.0).in_journal(other.id)).unwrap();

    store.delete_journal(mine.id).unwrap();

    // Both survive. A reading belongs to its tracker, which is a vault
    // record; the journal is only where it happened to be ticked, and
    // deleting the notebook you wrote in does not undo the run.
    let left = store.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(left.len(), 2, "readings outlive the journal they were logged in");

    let orphaned = left.iter().find(|r| r.journal_id.is_none()).expect("one was detached");
    assert_eq!(orphaned.value, 1.0);
    // The clear column too, or the index would still find it by that journal.
    let by_journal = ReadingQuery { journal_id: Some(mine.id), ..Default::default() };
    assert!(store.list_readings(&by_journal).unwrap().is_empty());
    // ...and the untouched one still names the journal it was ticked in.
    assert!(left.iter().any(|r| r.journal_id == Some(other.id)));
}

#[test]
fn deleting_an_entry_keeps_its_readings_but_drops_the_link() {
    // Deleting the paragraph about a run does not undo the run. What must
    // not survive is the pointer -- in *both* copies of it, the clear column
    // and the sealed payload, or a restore would resurrect a dead link.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let journal = Journal::new("Health");
    store.put_journal(&journal).unwrap();
    let entry = Entry::new(journal.id, "UTC");
    store.put_entry(&entry).unwrap();

    let t = karmayoga_core::TrackerId::new();
    let reading = Reading::on(t, a_day(1), 45.0).in_journal(journal.id).with_entry(entry.id);
    store.put_reading(&reading).unwrap();

    store.delete_entry(entry.id).unwrap();

    let kept = store.get_reading(reading.id).unwrap();
    assert_eq!(kept.value, 45.0, "the reading itself must survive");
    assert_eq!(kept.entry_id, None, "the sealed copy of the link must be cleared");
    // The clear column too, or the index would still find it by that entry.
    let by_entry = ReadingQuery { entry_id: Some(entry.id), ..Default::default() };
    assert!(store.list_readings(&by_entry).unwrap().is_empty());
}

#[test]
fn deleting_a_tracker_takes_only_its_own_readings() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let journal = Journal::new("Health");
    let (gone, kept) = (karmayoga_core::TrackerId::new(), karmayoga_core::TrackerId::new());
    store.put_reading(&Reading::on(gone, a_day(1), 1.0).in_journal(journal.id)).unwrap();
    store.put_reading(&Reading::on(gone, a_day(2), 1.0).in_journal(journal.id)).unwrap();
    store.put_reading(&Reading::on(kept, a_day(1), 1.0).in_journal(journal.id)).unwrap();

    assert_eq!(store.delete_readings_of(gone).unwrap(), 2);
    let left = store.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].tracker_id, kept);
}

#[test]
fn a_version_4_database_gains_the_readings_table_without_losing_entries() {
    // The migration people will actually run: a vault written before
    // tracking existed, opened by a build that has it. Readings arrive at
    // version 5 because the library took version 4 first -- the numbering
    // is a queue, and joining it in the wrong place would mean a vault
    // migrated by one build silently skipping the other's tables.
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::new("Health");
    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = RichDoc::from_plain_text("written before there were trackers");

    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_journal(&journal).unwrap();
        store.put_entry(&entry).unwrap();
    }
    {
        let conn = raw(dir.path());
        conn.execute_batch("DROP TABLE readings;").unwrap();
        conn.pragma_update(None, "user_version", 4i64).unwrap();
    }

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert_eq!(store.get_entry(entry.id).unwrap(), entry, "migrating must not disturb the journal");
    // A journal written by the older build has neither key, and both must
    // deserialise as "none" rather than as a failure.
    let back = store.get_journal(journal.id).unwrap();
    assert!(back.trackers.is_empty());
    assert!(back.shown_trackers.is_empty());

    let t = karmayoga_core::TrackerId::new();
    let reading = Reading::on(t, a_day(1), 1.0).in_journal(journal.id);
    store.put_reading(&reading).unwrap();
    assert_eq!(store.get_reading(reading.id).unwrap(), reading);
}

// ---- the read pool ------------------------------------------------------

/// A read must not wait behind an unrelated read.
///
/// The check that matters for a vault several clients can reach at once. It
/// is written as a rendezvous rather than as a timing comparison: two threads
/// each take a read, and each waits for the other to have taken one before
/// letting go. On the old single-connection store the second read could not
/// begin until the first had finished, so this deadlocks -- which is the
/// point. The timeout is what turns that deadlock into a failed assertion
/// instead of a hung test run.
#[test]
fn two_reads_can_be_in_flight_at_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    store.put_journal(&Journal::new("Daily")).unwrap();

    let inside = AtomicUsize::new(0);
    let both_arrived = std::thread::scope(|scope| {
        let reader = || {
            store
                .with_read(|_conn| {
                    inside.fetch_add(1, Ordering::SeqCst);
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while inside.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                        std::thread::yield_now();
                    }
                    Ok(inside.load(Ordering::SeqCst) == 2)
                })
                .unwrap()
        };
        let a = scope.spawn(reader);
        let b = scope.spawn(reader);
        a.join().unwrap() && b.join().unwrap()
    });

    assert!(both_arrived, "a read waited for an unrelated read to finish");
}

/// A write is still serialised, and a read taken afterwards sees it.
#[test]
fn a_write_is_visible_to_the_next_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    std::thread::scope(|scope| {
        for n in 0..8 {
            let store = &store;
            scope.spawn(move || {
                store.put_journal(&Journal::new(format!("J{n}"))).unwrap();
            });
        }
    });

    // Every write landed, and a reader connection -- a different session from
    // the one that wrote -- can see all of them.
    assert_eq!(store.list_journals().unwrap().len(), 8);
}

#[test]
fn the_database_file_contains_no_readable_note_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let mut note = Note::written("A note title nobody should see", "note body nobody should see");
    note.tags = vec!["secretnotetag".into()];
    note.pinned = true;
    store.put_note(&note).unwrap();
    store.flush().unwrap();

    // What *is* readable is the pin and the timestamps, which is what orders
    // the list -- and is the whole of the trade this table makes.
    let pinned: i64 =
        raw(dir.path()).query_row("SELECT pinned FROM notes", [], |r| r.get(0)).unwrap();
    assert_eq!(pinned, 1, "the pin is an index column and stays in the clear");

    let bytes = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"A note title nobody should see".as_slice(),
        b"note body nobody should see",
        b"secretnotetag",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn a_version_7_database_gains_notes_without_losing_goals() {
    // The migration people will actually run: a vault written before notes,
    // the profile and the assistant's routines existed, opened by a build
    // that has them.
    let dir = tempfile::tempdir().unwrap();
    let area = karmayoga_core::purpose::Area::new("this must survive the migration");

    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_area(&area).unwrap();
    }
    // Rewind to the world as version 7 left it: the new tables gone and the
    // recorded version behind.
    {
        let conn = raw(dir.path());
        conn.execute_batch(
            "DROP TABLE notes; DROP TABLE profile;
             DROP TABLE routines; DROP TABLE routine_runs;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 7i64).unwrap();
    }

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert_eq!(
        store.get_area(area.id).unwrap().name,
        "this must survive the migration",
        "migrating must not disturb what was already there"
    );

    let note = Note::written("and the new table must work", "with a body");
    store.put_note(&note).unwrap();
    assert_eq!(store.get_note(note.id).unwrap(), note);
}

#[test]
fn a_version_8_database_gains_mails_groundwork_without_losing_notes() {
    // The migration people will actually run once mail lands: a vault
    // written before this phase existed, opened by a build that has it.
    let dir = tempfile::tempdir().unwrap();
    let note = Note::written("this must survive the migration", "body");

    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_note(&note).unwrap();
    }
    // Rewind to the world as version 8 left it: the new tables gone and the
    // recorded version behind.
    {
        let conn = raw(dir.path());
        conn.execute_batch("DROP TABLE record_secrets; DROP TABLE mail_packs;").unwrap();
        conn.pragma_update(None, "user_version", 8i64).unwrap();
    }

    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    assert_eq!(
        store.get_note(note.id).unwrap().title,
        "this must survive the migration",
        "migrating must not disturb what was already there"
    );

    // The new tables must work: a secret round trips through `record_secrets`...
    store.put_secret("account", "acc-1", b"a token").unwrap();
    assert_eq!(
        store.get_secret("account", "acc-1").unwrap().as_deref(),
        Some(b"a token".as_slice())
    );

    // ...and `mail_packs` takes rows through `TablePacks`.
    let refs = TablePacks::new(&store).append_batch("acc-1", &[b"raw message".as_slice()]).unwrap();
    assert_eq!(TablePacks::new(&store).read(&refs[0]).unwrap(), b"raw message");
}

#[test]
fn a_secret_sealed_for_one_owner_cannot_be_opened_under_another() {
    // The conformance suite checks this through the trait; this reaches
    // underneath it, the way `a_database_written_under_one_key_does_not_open_under_another`
    // does for the vault as a whole: an attacker with database access but not
    // the vault's key must not be able to move a row's sealed bytes onto a
    // different owner's primary key and have it decrypt there.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    store.put_secret("account", "victim", b"the real secret").unwrap();
    store.put_secret("account", "attacker", b"whatever is already here").unwrap();

    let conn = raw(dir.path());
    let victim_data: Vec<u8> = conn
        .query_row(
            "SELECT data FROM record_secrets WHERE owner_kind = 'account' AND owner_id = 'victim'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "UPDATE record_secrets SET data = ?1 WHERE owner_kind = 'account' AND owner_id = 'attacker'",
        rusqlite::params![victim_data],
    )
    .unwrap();

    assert_eq!(store.get_secret("account", "attacker").unwrap_err().code(), "decrypt_failed");
    // The victim's own row is untouched and still opens normally.
    assert_eq!(
        store.get_secret("account", "victim").unwrap().as_deref(),
        Some(b"the real secret".as_slice())
    );
}

#[test]
fn the_table_backed_pack_store_passes_the_shared_conformance_suite() {
    // `TablePacks` is written for a vault with no local disk -- Postgres --
    // but it is plain SQL over `mail_packs`, so it is exercised here too:
    // one less thing that works only on the backend it was designed for.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    run_pack_store_suite(&TablePacks::new(&store), "acc-conformance");
}

/// A thread you replied to, on a vault written before a reply counted: it moves
/// up the lists it is filed in, but for Trash, Spam and Drafts. A thread with
/// something newer than the reply stays by that, and replaying the step (a
/// process that died before recording the version does) changes nothing.
#[test]
fn a_version_19_database_moves_threads_you_replied_to_up_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let account = AccountId::new();
    let inbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    let sent = Mailbox::new(account, "Sent", MailboxRole::Sent);
    let trash = Mailbox::new(account, "Trash", MailboxRole::Trash);
    let start = jiff::Timestamp::from_second(1_790_000_000).unwrap();
    let hours = |n: i64| start + jiff::SignedDuration::from_hours(n);
    let message = |thread: ThreadId, at: i64| Message {
        id: MailMessageId::new(),
        account_id: account,
        thread_id: thread,
        message_id_header: format!("<{}@example.com>", MailMessageId::new()),
        date: hours(at),
        from: Address::new("Somebody", "somebody@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Plans".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    let (answered, quiet, answered_back, trashed, sent_only) =
        (ThreadId::new(), ThreadId::new(), ThreadId::new(), ThreadId::new(), ThreadId::new());
    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_mailbox(&inbox).unwrap();
        store.put_mailbox(&sent).unwrap();
        store.put_mailbox(&trash).unwrap();
        let filed = [
            (answered, 0, &inbox, 1),
            (answered, 2, &sent, 1),
            (quiet, 1, &inbox, 2),
            (answered_back, 3, &sent, 2),
            (answered_back, 4, &inbox, 3),
            (trashed, 0, &trash, 1),
            (trashed, 5, &sent, 3),
            (sent_only, 6, &sent, 4),
        ];
        let ingest: Vec<IngestMessage> = filed
            .into_iter()
            .map(|(thread, at, mailbox, uid)| IngestMessage {
                message: message(thread, at),
                mailbox: mailbox.id,
                uid,
            })
            .collect();
        store.ingest(account, ingest).unwrap();
    }

    // What an earlier build left of the one row this rule has moved since.
    let rewind = || {
        let conn = raw(dir.path());
        conn.execute(
            "UPDATE thread_mailboxes SET last_date_us = ?1
             WHERE thread_id = ?2 AND mailbox_id = ?3",
            rusqlite::params![
                hours(0).as_microsecond(),
                answered.to_string(),
                inbox.id.to_string()
            ],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 19i64).unwrap();
    };
    let listed = |mailbox: &Mailbox| -> Vec<(ThreadId, jiff::Timestamp)> {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        let page = store.list_threads(mailbox.id, &ThreadFilter::default(), None, 50).unwrap();
        page.threads.iter().map(|t| (t.id, t.last_date)).collect()
    };
    let check = || {
        assert_eq!(
            listed(&inbox),
            vec![(answered_back, hours(4)), (answered, hours(2)), (quiet, hours(1))],
            "moved by the reply, or kept by something newer"
        );
        assert_eq!(listed(&trash), vec![(trashed, hours(0))], "Trash keeps to its own dates");
        assert_eq!(
            listed(&sent),
            vec![
                (sent_only, hours(6)),
                (trashed, hours(5)),
                (answered_back, hours(3)),
                (answered, hours(2))
            ],
            "Sent is already in this order"
        );
    };

    rewind();
    check();

    // Replayed over itself, as a crash before the version is recorded leaves it.
    raw(dir.path()).pragma_update(None, "user_version", 19i64).unwrap();
    check();
}

/// The list's Starred and Attachments filters on a vault written before they
/// had columns of their own to read: what is already starred or attached is
/// found without any of it being touched again, and replaying the step (a
/// process that died before recording the version does) changes nothing.
#[test]
fn a_version_18_database_fills_in_the_starred_and_attachment_columns() {
    let dir = tempfile::tempdir().unwrap();
    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    let message = |subject: &str, attachment: bool| Message {
        id: MailMessageId::new(),
        account_id: account,
        thread_id: ThreadId::new(),
        message_id_header: format!("<{subject}@example.com>"),
        date: jiff::Timestamp::now(),
        from: Address::new("Somebody", "somebody@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: attachment,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    let (plain, starred, attached) =
        (message("plain", false), message("starred", false), message("attached", true));
    let threads = (plain.thread_id, starred.thread_id, attached.thread_id);
    {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        store.put_mailbox(&mailbox).unwrap();
        let ingest: Vec<IngestMessage> = [plain, starred.clone(), attached]
            .into_iter()
            .enumerate()
            .map(|(i, message)| IngestMessage { message, mailbox: mailbox.id, uid: i as u32 + 1 })
            .collect();
        store.ingest(account, ingest).unwrap();
        let mut flags = starred.flags;
        flags.flagged = true;
        store.set_message_flags(starred.id, flags).unwrap();
    }

    let rewind = || {
        let conn = raw(dir.path());
        conn.execute_batch(
            "CREATE TABLE thread_mailboxes_old (
                 thread_id TEXT NOT NULL, mailbox_id TEXT NOT NULL,
                 last_date_us INTEGER NOT NULL, unread INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (thread_id, mailbox_id));
             INSERT INTO thread_mailboxes_old
                 SELECT thread_id, mailbox_id, last_date_us, unread FROM thread_mailboxes;
             DROP TABLE thread_mailboxes;
             ALTER TABLE thread_mailboxes_old RENAME TO thread_mailboxes;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 18i64).unwrap();
    };
    let check = || {
        let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
        let listed = |filter: ThreadFilter| -> Vec<ThreadId> {
            let page = store.list_threads(mailbox.id, &filter, None, 50).unwrap();
            page.threads.iter().map(|t| t.id).collect()
        };
        assert_eq!(
            listed(ThreadFilter { starred: Some(true), ..Default::default() }),
            vec![threads.1]
        );
        assert_eq!(
            listed(ThreadFilter { has_attachments: Some(true), ..Default::default() }),
            vec![threads.2]
        );
        assert_eq!(listed(ThreadFilter::default()).len(), 3, "nothing was lost in the rebuild");
    };

    rewind();
    check();

    // Replayed over itself, as a crash before the version is recorded leaves it.
    let conn = raw(dir.path());
    conn.pragma_update(None, "user_version", 18i64).unwrap();
    drop(conn);
    check();
}

/// A draft on the server seldom carries `\Seen`, and used to make its thread
/// look unread. It is not counted now, and a count stored before that is put
/// right once, by `recount_draft_unread`.
#[test]
fn a_draft_does_not_make_its_thread_unread() {
    let dir = tempfile::tempdir().unwrap();
    let account = AccountId::new();
    let inbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    let drafts = Mailbox::new(account, "Drafts", MailboxRole::Drafts);
    let thread = ThreadId::new();
    let message = |subject: &str, flags: MessageFlags| Message {
        id: MailMessageId::new(),
        account_id: account,
        thread_id: thread,
        message_id_header: format!("<{subject}@example.com>"),
        date: jiff::Timestamp::now(),
        from: Address::new("Somebody", "somebody@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: String::new(),
        flags,
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    let read = message("read", MessageFlags { seen: true, ..Default::default() });
    let draft = message("draft", MessageFlags { draft: true, ..Default::default() });
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    store.put_mailbox(&inbox).unwrap();
    store.put_mailbox(&drafts).unwrap();
    store
        .ingest(
            account,
            vec![
                IngestMessage { message: read, mailbox: inbox.id, uid: 1 },
                IngestMessage { message: draft, mailbox: drafts.id, uid: 1 },
            ],
        )
        .unwrap();
    let unread = |store: &dyn MailStore| -> (u32, u64) {
        let (t, _) = store.thread(thread).unwrap();
        let counts = store.unread_counts(account).unwrap();
        (t.unread_count, counts.iter().map(|(_, n)| n).sum())
    };
    assert_eq!(unread(&store), (0, 0), "the draft was counted as unread");

    // As a count stored before drafts were left out reads.
    let stale = || {
        raw(dir.path())
            .execute_batch("UPDATE threads SET unread = 1; UPDATE thread_mailboxes SET unread = 1;")
            .unwrap()
    };
    stale();
    assert_eq!(store.recount_draft_unread(account).unwrap(), 1);
    assert_eq!(unread(&store), (0, 0));
    stale();
    assert_eq!(store.recount_draft_unread(account).unwrap(), 0, "done once, then not scanned");
}

#[test]
fn deleting_an_account_leaves_no_row_in_any_account_keyed_mail_table() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    karmayoga_store_sql::accounts::run_account_delete_cascade_regression(&store);
}

#[test]
fn recategorize_does_not_clobber_a_concurrent_flag_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    karmayoga_store_sql::mail::run_recategorize_staleness_regression(&store);
}

#[test]
fn the_database_file_contains_no_readable_mail_text() {
    // What the mail tables promise, checked the way every other domain's
    // promise is: write a subject, a sender's name and a label nobody
    // should see, then grep the raw file for them. What is deliberately
    // readable is the thread's clear `unread` and `last_date_us` columns --
    // the plan's own words -- so this also checks that at least one of them
    // survives the round trip in the clear, or the test would be trivially
    // satisfied by a backend that sealed everything and broke the inbox.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "A folder name nobody should see", MailboxRole::Inbox);
    store.put_mailbox(&mailbox).unwrap();

    let thread_id = ThreadId::new();
    let message_id = MailMessageId::new();
    let message = Message {
        id: message_id,
        account_id: account,
        thread_id,
        message_id_header: "<secretmsgid@example.com>".into(),
        date: jiff::Timestamp::now(),
        from: Address::new("A sender name nobody should see", "sender@example.com"),
        to: vec![Address::bare("recipient@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "A subject line nobody should see".into(),
        snippet: "a snippet nobody should see".into(),
        flags: MessageFlags::default(),
        labels: vec!["A label name nobody should see".into()],
        has_attachments: false,
        size: 42,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    store.ingest(account, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }]).unwrap();
    store.flush().unwrap();

    let bytes = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"A folder name nobody should see".as_slice(),
        b"A sender name nobody should see",
        b"sender@example.com",
        b"recipient@example.com",
        b"A subject line nobody should see",
        b"a snippet nobody should see",
        b"A label name nobody should see",
        b"secretmsgid@example.com",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }

    // What *is* readable: a thread's `unread` and `last_date_us` clear
    // columns, checked directly against the raw file's `thread_mailboxes`
    // row rather than through the store, the same way
    // `the_database_file_contains_no_readable_note_text` checks `pinned`.
    let conn = raw(dir.path());
    let unread: i64 = conn
        .query_row(
            "SELECT unread FROM thread_mailboxes WHERE thread_id = ?1 AND mailbox_id = ?2",
            rusqlite::params![thread_id.to_string(), mailbox.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unread, 1, "a fresh message's unread count is meant to be readable");

    // And the store's own read path agrees with what was just written.
    let page = store.list_threads(mailbox.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(page.threads.len(), 1);
    assert_eq!(page.threads[0].subject, "A subject line nobody should see");
}

#[test]
fn deleting_a_note_takes_its_transcript_and_clears_the_recording_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = BackendRegistry::new();
    registry.register(SqliteFactory);
    let vault = Vault::create(dir.path(), VaultConfig::default(), Arc::new(registry)).unwrap();

    let note = Note::new("Design sync");
    vault.save_note(&note, None).unwrap();

    let mut recording = Recording::new("Design sync", None, TemplateId::new());
    recording.stage = Stage::Done;
    recording.note_id = Some(note.id);
    vault.save_recording(&recording).unwrap();

    let now = jiff::Timestamp::now();
    let transcript = Transcript {
        id: TranscriptId::new(),
        note_id: note.id,
        recording_id: Some(recording.id),
        language: None,
        backend: "local:parakeet-tdt-0.6b-v3".into(),
        speakers: Vec::new(),
        segments: vec![karmayoga_core::meeting::Segment {
            start_ms: 0,
            end_ms: 1_000,
            speaker: 0,
            text: "let's ship on friday".into(),
        }],
        room: false,
        created_at: now,
        updated_at: now,
    };
    vault.save_transcript(&transcript).unwrap();

    // Findable through the note before it is deleted -- the whole point of
    // folding a transcript's words into its note's search entry.
    let hits = vault.search("friday", SearchScope::Everything, 10).unwrap();
    assert_eq!(hits.len(), 1, "the transcript's words must be searchable through the note");

    vault.delete_note(note.id).unwrap();

    assert!(vault.transcript(transcript.id).is_err(), "the transcript must go with the note");
    assert!(
        vault.transcript_for_note(note.id).unwrap().is_none(),
        "and the note no longer has one"
    );
    let after = vault.recording(recording.id).unwrap();
    assert_eq!(after.note_id, None, "the recording's pointer back to the note must be cleared");

    let hits = vault.search("friday", SearchScope::Everything, 10).unwrap();
    assert!(hits.is_empty(), "deleted words must not still be findable");
}

#[test]
fn an_autosave_keeps_the_note_s_transcript_searchable() {
    // `save_note` used to reindex with the note's own words alone --
    // `u.index.insert_note`, not `reindex_note` -- so an autosave, or
    // `name_speaker` rewriting the body, threw the transcript half of the
    // search entry away until the vault was next unlocked and
    // `rebuild_meeting_index` ran. A plain save must go through the same
    // path `save_transcript` does.
    let dir = tempfile::tempdir().unwrap();
    let mut registry = BackendRegistry::new();
    registry.register(SqliteFactory);
    let vault = Vault::create(dir.path(), VaultConfig::default(), Arc::new(registry)).unwrap();

    let note = Note::new("Design sync");
    vault.save_note(&note, None).unwrap();

    let now = jiff::Timestamp::now();
    let transcript = Transcript {
        id: TranscriptId::new(),
        note_id: note.id,
        recording_id: None,
        language: None,
        backend: "local:parakeet-tdt-0.6b-v3".into(),
        speakers: Vec::new(),
        segments: vec![karmayoga_core::meeting::Segment {
            start_ms: 0,
            end_ms: 1_000,
            speaker: 0,
            text: "let's ship on friday".into(),
        }],
        room: false,
        created_at: now,
        updated_at: now,
    };
    vault.save_transcript(&transcript).unwrap();
    assert_eq!(vault.search("friday", SearchScope::Everything, 10).unwrap().len(), 1);

    // An ordinary autosave of the note body, unrelated to the transcript --
    // exactly the path that used to drop the transcript's words.
    let mut edited = note.clone();
    edited.body = RichDoc::from_plain_text("action items");
    edited.updated_at = jiff::Timestamp::now();
    vault.save_note(&edited, Some(note.updated_at)).unwrap();

    assert_eq!(
        vault.search("friday", SearchScope::Everything, 10).unwrap().len(),
        1,
        "a plain note save must not drop the transcript's words from search"
    );
}

#[test]
fn the_database_file_contains_no_readable_meeting_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let mut recording =
        Recording::new("A meeting title nobody should see", None, TemplateId::new());
    recording.stage = Stage::Done;
    store.put_recording(&recording).unwrap();

    let now = jiff::Timestamp::now();
    let transcript = Transcript {
        id: TranscriptId::new(),
        note_id: NoteId::new(),
        recording_id: Some(recording.id),
        language: None,
        backend: "local:parakeet-tdt-0.6b-v3".into(),
        speakers: vec![Speaker {
            key: 0,
            label: "A speaker name nobody should see".into(),
            email: None,
            voiceprint_id: None,
            how: Attribution::Owner,
            centroid: Vec::new(),
            embedding_model: String::new(),
        }],
        segments: vec![Segment {
            start_ms: 0,
            end_ms: 1_000,
            speaker: 0,
            text: "a word nobody should see".into(),
        }],
        room: false,
        created_at: now,
        updated_at: now,
    };
    store.put_transcript(&transcript).unwrap();

    let voiceprint = Voiceprint {
        id: VoiceprintId::new(),
        name: "A voiceprint name nobody should see".into(),
        email: None,
        is_owner: false,
        model: "3d-speaker".into(),
        centroids: vec![vec![0.1, 0.2]],
        samples: 1,
        created_at: now,
        updated_at: now,
    };
    store.put_voiceprint(&voiceprint).unwrap();
    store.flush().unwrap();

    // What is readable: a recording's `stage` -- checked directly, the same
    // way the note suite checks `pinned`.
    let stage: String =
        raw(dir.path()).query_row("SELECT stage FROM recordings", [], |r| r.get(0)).unwrap();
    assert_eq!(stage, "done", "the stage is an index column and stays in the clear");

    let bytes = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"A meeting title nobody should see".as_slice(),
        b"A speaker name nobody should see",
        b"a word nobody should see",
        b"A voiceprint name nobody should see",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn the_database_file_contains_no_readable_money_text() {
    // What somebody earns and spends, and the provider ids that would let a
    // stranger line their file up against a bank's, must never sit in a
    // clear column. What is meant to be readable is a transaction's day and
    // its *keyed* dedupe hash -- see `karmayoga_store_sql::schema`'s `v22`.
    use karmayoga_core::money::{
        AccountKind, BalanceSnapshot, BankAccount, BankLink, LinkProvider, LinkState,
        MoneyCategory, MoneyRule, PlaidEnvironment, Txn,
    };
    use karmayoga_core::store::money::MoneyStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let link = BankLink::new(
        LinkProvider::Plaid {
            item_id: "item-nobody-should-see".into(),
            institution_id: "ins_nobodyshouldsee".into(),
            environment: PlaidEnvironment::Sandbox,
        },
        "A bank nobody should see",
    );
    store.put_link(&link).unwrap();
    let mut state = LinkState::new(link.id);
    state.cursor = "cursor-nobody-should-see".into();
    state.last_error = Some("an error nobody should see".into());
    store.put_link_state(&state).unwrap();

    let mut account = BankAccount::manual("An account nobody should see", AccountKind::Cash, "USD");
    account.link_id = Some(link.id);
    account.provider_account_id = "acct-nobody-should-see".into();
    account.official_name = "An official name nobody should see".into();
    // Longer than a real mask, so it cannot turn up by chance in the hex of
    // an id in a clear column.
    account.mask = "913782".into();
    account.balance = Some(8_675_309);
    store.put_account(&account).unwrap();
    store
        .put_balance(&BalanceSnapshot {
            account_id: account.id,
            day: jiff::civil::date(2026, 11, 5),
            balance: 8_675_309,
            available: Some(8_675_308),
        })
        .unwrap();

    let mut txn = Txn::new(
        account.id,
        "a3f0c9d28e7b4f5e9d0e2c7f8a6b5d4e",
        jiff::civil::date(2026, 11, 5),
        -4_242_424,
        "USD",
    );
    txn.provider_id = "txn-nobody-should-see".into();
    txn.payee = "A payee nobody should see".into();
    txn.description = "A memo nobody should see".into();
    txn.note = "A note nobody should see".into();
    txn.bank_category = "FOOD_AND_DRINK/FOOD_AND_DRINK_NOBODY".into();
    store.put_txns(std::slice::from_ref(&txn)).unwrap();

    let mut category = MoneyCategory::new(
        "A category nobody should see",
        "A group nobody should see",
        karmayoga_core::money::CategoryKind::Expense,
    );
    category.budget = Some(1_234_567);
    store.put_category(&category).unwrap();
    let mut rule = MoneyRule::new("rule text nobody should see");
    rule.rename = "a rename nobody should see".into();
    store.put_rule(&rule).unwrap();
    store.flush().unwrap();

    let raw = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"nobody should see".as_slice(),
        b"nobodyshouldsee",
        b"nobody-should-see",
        b"NOBODY",
        b"913782",
        b"8675309",
        b"86753.09",
        b"4242424",
        b"42424.24",
        b"1234567",
        b"FOOD_AND_DRINK",
    ] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
    assert!(raw.windows(10).any(|w| w == b"2026-11-05"), "the day column is meant to be readable");
}

#[test]
fn the_database_file_contains_no_readable_file_path_name_or_hash() {
    // The files folder may sit on an encrypted volume that is locked while
    // somebody holds a copy of this database, so nothing a person would
    // recognise of a file -- its path, its folder, its name, its content
    // hash, a caption or a tag -- may sit in a clear column, plain file or
    // sealed. What is meant to be readable is a token of each, under a key
    // only the vault has. See `karmayoga_store_sql::files`.
    use karmayoga_core::files::{
        FileEntry, FileKind, FileOrigin, FileStatus, FileStorage, MediaInfo,
    };
    use karmayoga_core::id::FileId;
    use karmayoga_core::store::files::FilesStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let now = jiff::Timestamp::now();
    let hash = "a3f0c9d28e7b4f5e9d0e2c7f8a6b5d4ea3f0c9d28e7b4f5e9d0e2c7f8a6b5d4e";
    let file = |storage: FileStorage, name: &str| FileEntry {
        id: FileId::new(),
        storage,
        name: name.into(),
        kind: FileKind::Document,
        mime: "application/pdf".into(),
        size: 4_096,
        hash: hash.into(),
        modified_at: now,
        added_at: now,
        updated_at: now,
        origin: FileOrigin::Files,
        status: FileStatus::Present,
        media: MediaInfo::default(),
        thumbnail: None,
        caption: "A caption nobody should see".into(),
        tags: vec!["tag-nobody-should-see".into()],
        rating: None,
        favourite: false,
        hidden: false,
        people: Vec::new(),
        place: None,
        stack: None,
        properties: Default::default(),
    };
    let plain = file(
        FileStorage::Plain {
            path: "LawyerNobodyShouldSee/settlement-nobody-should-see.pdf".into(),
        },
        "settlement-nobody-should-see.pdf",
    );
    let sealed = file(
        FileStorage::Sealed { blob: store.put_blob(b"sealed contents").unwrap() },
        "sealed-nobody-should-see.pdf",
    );
    store.put_files(&[plain.clone(), sealed]).unwrap();
    store.flush().unwrap();

    let raw = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"nobody should see".as_slice(),
        b"nobody-should-see",
        b"NobodyShouldSee",
        b"settlement",
        hash.as_bytes(),
    ] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
    // And the tokens still answer what the indexes ask.
    let found = store.file_by_path("LawyerNobodyShouldSee/settlement-nobody-should-see.pdf");
    assert_eq!(found.unwrap().map(|f| f.id), Some(plain.id));
    assert_eq!(
        store.files_by_hash(hash).unwrap().len(),
        1,
        "only the plain file's hash is a token"
    );
}

/// A span on the laptop, with something nobody should see in every field a
/// person would recognise. For the two screen-time tests below.
fn a_screen_span(start: i64, end: i64) -> karmayoga_core::screentime::ScreenSpan {
    use karmayoga_core::screentime::{ScreenSpan, SpanKind};
    ScreenSpan {
        id: karmayoga_core::ScreenSpanId::new(),
        device: "a-device-nobody-should-see".into(),
        collector: "collectornobodyshouldsee".into(),
        kind: SpanKind::Site,
        app: "a-browser-nobody-should-see".into(),
        name: "A browser name nobody should see".into(),
        host: "a-host-nobody-should-see.example".into(),
        title: "A tab title nobody should see".into(),
        start: jiff::Timestamp::from_second(start).unwrap(),
        end: jiff::Timestamp::from_second(end).unwrap(),
    }
}

#[test]
fn the_database_file_contains_no_readable_screen_time() {
    // Which machine, which app, which site, which tab: none of it may sit in
    // a clear column. What is meant to be readable is when a span started and
    // ended, and the stream's *keyed* hash -- see `karmayoga_store_sql::schema`'s
    // `v25`.
    use karmayoga_core::screentime::{ScreenApp, ScreenCategory, ScreenTimeSettings, SpanKind};
    use karmayoga_core::store::screentime::ScreenTimeStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();

    let stream = "0123456789abcdef0123456789abcdef";
    store.put_spans(stream, &[a_screen_span(1_800_000_000, 1_800_000_600)]).unwrap();

    let mut category = ScreenCategory::new("A category nobody should see");
    category.color = "#a1b2c3".into();
    category.builtin = "builtinnobodyshouldsee".into();
    store.put_category(&category).unwrap();
    let mut label = ScreenApp::new("site:a-label-key-nobody-should-see.example", SpanKind::Site);
    label.name = "A label nobody should see".into();
    label.category = Some(category.id);
    store.put_app(&label).unwrap();
    store
        .put_screen_settings(&ScreenTimeSettings {
            browsers: vec!["a-browser-setting-nobody-should-see".into()],
            ..ScreenTimeSettings::default()
        })
        .unwrap();
    store.flush().unwrap();

    // What is readable: the stream, exactly as the vault handed it over.
    let held: String =
        raw(dir.path()).query_row("SELECT stream FROM screen_spans", [], |r| r.get(0)).unwrap();
    assert_eq!(held, stream, "the stream is an index column and stays in the clear");

    let bytes = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"nobody should see".as_slice(),
        b"nobodyshouldsee",
        b"nobody-should-see",
        b"#a1b2c3",
        b"site:",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn a_screen_span_copied_into_another_row_does_not_open_there() {
    // Somebody who can write the file but does not hold the key moves one
    // span's sealed payload into another's row. Bound to its own id, it will
    // not open under the other's.
    use karmayoga_core::store::screentime::ScreenTimeStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let first = a_screen_span(1_800_000_000, 1_800_000_060);
    let second = a_screen_span(1_800_000_100, 1_800_000_160);
    store.put_spans("stream", &[first.clone(), second.clone()]).unwrap();
    store.flush().unwrap();
    let from = jiff::Timestamp::from_second(1_799_999_000).unwrap();
    let to = jiff::Timestamp::from_second(1_800_001_000).unwrap();
    assert_eq!(store.spans(from, to).unwrap(), vec![first.clone(), second.clone()]);

    raw(dir.path())
        .execute(
            "UPDATE screen_spans SET data = (SELECT data FROM screen_spans WHERE id = ?1)
             WHERE id = ?2",
            [first.id.to_string(), second.id.to_string()],
        )
        .unwrap();

    let err = store.spans(from, to).unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
    let err = store.last_span("stream").unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
}

/// A series' keyed hash, as the vault hands it over: opaque to the store.
const HEALTH_SERIES: &str = "7a1b2c3d4e5f60718293a4b5c6d7e8f9";

/// One day of glucose under `series`, with a device nobody should see and a
/// number that would be easy to find. For the two health tests below.
fn a_health_day(
    series: &str,
    day: jiff::civil::Date,
    value: f64,
) -> karmayoga_core::store::health::SeriesRow {
    use karmayoga_core::health::{Bucket, Metric, Sample, SeriesDay, SeriesSummary, Tombstone};
    karmayoga_core::store::health::SeriesRow {
        series: series.into(),
        day,
        data: SeriesDay {
            metric: Some(Metric::BloodGlucose),
            day: Some(day),
            sources: vec!["A device nobody should see".into()],
            samples: vec![Sample {
                start: 1_800_000_000,
                secs: 0,
                value,
                source: 0,
                offset_min: 0,
            }],
            tombstones: vec![Tombstone { start: 1_800_000_060, secs: 0, value: value + 1.0 }],
        },
        summary: SeriesSummary {
            metric: Some(Metric::BloodGlucose),
            day: Some(day),
            buckets: vec![Bucket {
                quarter: 1,
                count: 1,
                sum: value,
                min: value,
                max: value,
                last: value,
                covered: 0,
            }],
            offset_min: 0,
        },
    }
}

/// A medication with something nobody should see in every field a person
/// would recognise.
fn a_health_medication(name: &str) -> karmayoga_core::health::Medication {
    use karmayoga_core::health::{MedKind, MedSchedule, MedStatus, Medication};
    let now = jiff::Timestamp::from_second(1_800_000_000).unwrap();
    Medication {
        id: karmayoga_core::id::MedicationId::new(),
        name: name.into(),
        kind: MedKind::Prescription,
        form: "a form nobody should see".into(),
        strength: "a strength nobody should see".into(),
        dose: 4_321.5,
        dose_unit: "a unit nobody should see".into(),
        schedule: MedSchedule::AsNeeded,
        rules: Vec::new(),
        tags: vec!["a tag nobody should see".into()],
        status: MedStatus::Active,
        started: None,
        stopped: None,
        stop_reason: "a reason nobody should see".into(),
        history: Vec::new(),
        condition_ids: Vec::new(),
        prescriber: "A prescriber nobody should see".into(),
        pharmacy: "A pharmacy nobody should see".into(),
        supply: None,
        remind: true,
        codes: Vec::new(),
        source_names: vec!["a source name nobody should see".into()],
        notes: "A note nobody should see".into(),
        sort_order: 0,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn the_database_file_contains_no_readable_health_data() {
    // Which metric, what it read, which device, which medication, which
    // diagnosis, which test and what it found, which paper, which allergy,
    // which food: none of it may sit in a clear column. What is meant to be
    // readable is a series' keyed hash and its day, when a workout began and
    // its keyed key, which medication a dose is of, and the dates of a lab
    // result and a document -- see `karmayoga_store_sql::schema`'s `v30`.
    use karmayoga_core::health::{
        Activity, Allergy, AllergyKind, CarePlan, Condition, ConditionStatus, Connector, DocKind,
        Dose, DoseOrigin, DoseStatus, ExtractionState, Food, FoodSource, HealthDocument,
        HealthSettings, HealthSource, LabFlag, LabOrigin, LabResult, Meal, MealItem, MealSlot,
        Nutrients, Severity, SourceStatus, Workout,
    };
    use karmayoga_core::id::{
        AllergyId, ConditionId, DoseId, FoodId, HealthDocumentId, HealthSourceId, LabResultId,
        MealId, WorkoutId,
    };
    use karmayoga_core::store::health::HealthStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let now = jiff::Timestamp::from_second(1_800_000_000).unwrap();
    let later = jiff::Timestamp::from_second(1_800_001_800).unwrap();

    store
        .put_series(&[a_health_day(HEALTH_SERIES, jiff::civil::date(2026, 11, 5), 123_456.789)])
        .unwrap();
    store
        .put_health_settings(&HealthSettings {
            priority: vec!["A device nobody should see".into()],
            ..HealthSettings::default()
        })
        .unwrap();
    store
        .put_source(&HealthSource {
            id: HealthSourceId::new(),
            connector: Connector::HealthAutoExport,
            name: "A source nobody should see".into(),
            paused: true,
            status: SourceStatus {
                last_error: "an error nobody should see".into(),
                devices: vec!["A device nobody should see".into()],
                ..SourceStatus::default()
            },
            token_id: "a-token-id-nobody-should-see".into(),
            created_at: now,
            updated_at: now,
        })
        .unwrap();

    let workout_key = "a3f0c9d28e7b4f5e9d0e2c7f8a6b5d4e";
    store
        .put_workouts(&[Workout {
            id: WorkoutId::new(),
            activity: Activity::Running,
            name: "A workout nobody should see".into(),
            start: now,
            end: later,
            offset_min: -480,
            active_energy_kcal: Some(987_654.321),
            total_energy_kcal: None,
            distance_m: None,
            elevation_up_m: None,
            avg_hr: None,
            max_hr: None,
            effort: None,
            indoor: None,
            source: "A workout source nobody should see".into(),
            source_id: None,
            external_id: "external-nobody-should-see".into(),
            route: None,
            note: "A workout note nobody should see".into(),
            key: workout_key.into(),
            hidden: false,
            created_at: now,
            updated_at: now,
        }])
        .unwrap();

    let medication = a_health_medication("A medication nobody should see");
    store.put_medication(&medication).unwrap();
    let dose_key = "b4a1d0e39f8c5a6f0e1d3c8a9b7c6e5f";
    store
        .put_doses(&[Dose {
            id: DoseId::new(),
            medication_id: medication.id,
            scheduled_at: None,
            at: now,
            status: DoseStatus::Taken,
            amount: Some(4_321.5),
            origin: DoseOrigin::Source,
            note: "A dose note nobody should see".into(),
            key: dose_key.into(),
            hidden: false,
            created_at: now,
            updated_at: now,
        }])
        .unwrap();

    store
        .put_condition(&Condition {
            id: ConditionId::new(),
            name: "A condition nobody should see".into(),
            template: None,
            status: ConditionStatus::Active,
            diagnosed: None,
            codes: Vec::new(),
            plan: CarePlan::default(),
            notes: "A condition note nobody should see".into(),
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    store
        .put_labs(&[LabResult {
            id: LabResultId::new(),
            code: "code-nobody-should-see".into(),
            name: "A test nobody should see".into(),
            value: Some(7_531.246),
            text: "A finding nobody should see".into(),
            unit: "a unit nobody should see".into(),
            ref_low: None,
            ref_high: None,
            ref_text: String::new(),
            flag: LabFlag::High,
            collected: jiff::civil::date(2026, 11, 3),
            collected_at: None,
            lab: "A laboratory nobody should see".into(),
            panel: "A panel nobody should see".into(),
            document_id: None,
            origin: LabOrigin::Manual,
            note: String::new(),
            created_at: now,
            updated_at: now,
        }])
        .unwrap();
    store
        .put_document(&HealthDocument {
            id: HealthDocumentId::new(),
            title: "A document nobody should see".into(),
            kind: DocKind::LabReport,
            date: Some(jiff::civil::date(2026, 11, 4)),
            provider: "A clinic nobody should see".into(),
            files: Vec::new(),
            source: None,
            text: "A sentence of a report nobody should see".into(),
            fhir: Vec::new(),
            extraction: ExtractionState::None,
            extraction_error: String::new(),
            pending_labs: Vec::new(),
            condition_ids: Vec::new(),
            external_id: "fhir-nobody-should-see".into(),
            notes: String::new(),
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    store
        .put_allergy(&Allergy {
            id: AllergyId::new(),
            substance: "An allergen nobody should see".into(),
            kind: AllergyKind::Drug,
            reaction: "A reaction nobody should see".into(),
            severity: Severity::Severe,
            noted: None,
            notes: String::new(),
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    let nutrients = Nutrients { kcal: 8_642.0, ..Nutrients::default() };
    store
        .put_food(&Food {
            id: FoodId::new(),
            name: "A food nobody should see".into(),
            brand: "A brand nobody should see".into(),
            barcode: "barcode-nobody-should-see".into(),
            source: FoodSource::Own,
            per_100g: nutrients.clone(),
            servings: Vec::new(),
            favorite: false,
            last_used: Some(now),
            use_count: 1,
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    store
        .put_meal(&Meal {
            id: MealId::new(),
            at: now,
            offset_min: 0,
            slot: MealSlot::Dinner,
            name: "A meal nobody should see".into(),
            items: vec![MealItem {
                food_id: None,
                name: "An item nobody should see".into(),
                amount: 1.0,
                unit: "unit".into(),
                grams: None,
                nutrients,
                estimated: false,
            }],
            photo: None,
            note: "A meal note nobody should see".into(),
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    store.flush().unwrap();

    // What is readable: the series, as the vault handed it over, and its
    // day; a workout's keyed hash; which medication a dose is of; and the
    // two dates.
    let held = |sql: &str| -> String { raw(dir.path()).query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(held("SELECT series FROM health_series"), HEALTH_SERIES);
    assert_eq!(held("SELECT day FROM health_series"), "2026-11-05");
    assert_eq!(held("SELECT dedupe_key FROM health_workouts"), workout_key);
    assert_eq!(held("SELECT dedupe_key FROM health_doses"), dose_key);
    assert_eq!(held("SELECT medication_id FROM health_doses"), medication.id.to_string());
    assert_eq!(held("SELECT collected FROM health_labs"), "2026-11-03");
    assert_eq!(held("SELECT doc_date FROM health_documents"), "2026-11-04");

    let bytes = std::fs::read(dir.path().join(DB_FILENAME)).unwrap();
    for needle in [
        b"nobody should see".as_slice(),
        b"nobody-should-see",
        // The metric, by the name it is stored under and the one it is shown
        // by -- "this vault measures glucose" is itself a diagnosis.
        b"blood_glucose",
        b"bloodGlucose",
        b"BloodGlucose",
        // Values, as the JSON would write them.
        b"123456.789",
        b"987654.321",
        b"7531.246",
        b"4321.5",
        b"8642.0",
        // An activity, a connector, a flag and a severity, by name.
        b"running",
        b"healthAutoExport",
        b"\"severe\"",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "found {:?} in the database file",
            String::from_utf8_lossy(needle)
        );
    }
    assert!(bytes.windows(10).any(|w| w == b"2026-11-05"), "the day is meant to be readable");
}

#[test]
fn a_health_series_day_moved_to_another_series_day_or_half_does_not_open() {
    // Somebody who can write the file but does not hold the key moves a
    // sealed payload somewhere else. A day's samples are bound to their
    // series and their day, and the summary beside them to the same under a
    // label of its own, so neither opens as another day's, another series',
    // or the other half of the same row.
    use karmayoga_core::store::health::HealthStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let day = |n: i8| jiff::civil::date(2026, 11, n);
    let other = "another-series-hash";
    store
        .put_series(&[
            a_health_day(HEALTH_SERIES, day(1), 100.0),
            a_health_day(HEALTH_SERIES, day(2), 110.0),
            a_health_day(HEALTH_SERIES, day(3), 120.0),
            a_health_day(HEALTH_SERIES, day(4), 130.0),
            a_health_day(other, day(1), 140.0),
        ])
        .unwrap();
    store.flush().unwrap();
    let read_all = || store.series_days(HEALTH_SERIES, day(1), day(4));
    assert_eq!(read_all().unwrap().len(), 4);

    let db = raw(dir.path());
    // One day's samples moved to the next day of the same series.
    db.execute(
        "UPDATE health_series SET data = (SELECT data FROM health_series WHERE series = ?1 AND day = ?2)
         WHERE series = ?1 AND day = ?3",
        rusqlite::params![HEALTH_SERIES, "2026-11-01", "2026-11-02"],
    )
    .unwrap();
    let err = store.series_days(HEALTH_SERIES, day(2), day(2)).unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
    assert!(
        store.series_summaries(HEALTH_SERIES, day(2), day(2)).is_ok(),
        "the summary beside it was not touched, and opens"
    );

    // The same day's samples moved to another series.
    db.execute(
        "UPDATE health_series SET data = (SELECT data FROM health_series WHERE series = ?1 AND day = ?2)
         WHERE series = ?3 AND day = ?2",
        rusqlite::params![HEALTH_SERIES, "2026-11-01", other],
    )
    .unwrap();
    let err = store.series_days(other, day(1), day(1)).unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");

    // A day's summary put where its samples go, and the samples where the
    // summary goes: the two labels differ, so neither stands in for the
    // other.
    db.execute(
        "UPDATE health_series SET data = summary WHERE series = ?1 AND day = ?2",
        rusqlite::params![HEALTH_SERIES, "2026-11-03"],
    )
    .unwrap();
    let err = store.series_days(HEALTH_SERIES, day(3), day(3)).unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
    db.execute(
        "UPDATE health_series SET summary = data WHERE series = ?1 AND day = ?2",
        rusqlite::params![HEALTH_SERIES, "2026-11-04"],
    )
    .unwrap();
    let err = store.series_summaries(HEALTH_SERIES, day(4), day(4)).unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
    assert_eq!(store.series_days(HEALTH_SERIES, day(4), day(4)).unwrap().len(), 1);
}

#[test]
fn reading_summaries_never_opens_the_samples() {
    // The point of the second sealed column: a range of summaries is answered
    // without a single sample payload being decrypted. Proved the only way
    // the file allows -- by ruining every payload in `data` and reading the
    // range anyway.
    use karmayoga_core::store::health::HealthStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let day = |n: i8| jiff::civil::date(2026, 11, n);
    let rows: Vec<_> =
        (1..=5).map(|n| a_health_day(HEALTH_SERIES, day(n), 100.0 + f64::from(n))).collect();
    store.put_series(&rows).unwrap();
    store.flush().unwrap();

    raw(dir.path()).execute("UPDATE health_series SET data = x'00'", []).unwrap();

    let summaries = store.series_summaries(HEALTH_SERIES, day(1), day(5)).unwrap();
    assert_eq!(summaries, rows.iter().map(|r| r.summary.clone()).collect::<Vec<_>>());
    let err = store.series_days(HEALTH_SERIES, day(1), day(5)).unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
}

#[test]
fn a_health_record_copied_into_another_row_does_not_open_there() {
    // A medication's payload moved into another medication's row. Bound to
    // its own id, it will not open under the other's.
    use karmayoga_core::store::health::HealthStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let first = a_health_medication("The first medication");
    let second = a_health_medication("The second medication");
    store.put_medication(&first).unwrap();
    store.put_medication(&second).unwrap();
    store.flush().unwrap();
    assert_eq!(store.list_medications().unwrap().len(), 2);

    raw(dir.path())
        .execute(
            "UPDATE health_medications SET data = (SELECT data FROM health_medications WHERE id = ?1)
             WHERE id = ?2",
            [first.id.to_string(), second.id.to_string()],
        )
        .unwrap();

    let err = store.get_medication(second.id).unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
    let err = store.list_medications().unwrap_err();
    assert_eq!(err.code(), "decrypt_failed", "{err:?}");
    assert_eq!(store.get_medication(first.id).unwrap(), first, "the source row is as it was");
}

#[test]
fn deleting_a_medication_takes_its_doses_in_one_transaction() {
    // `delete_medication` deletes the doses by their clear `medication_id`,
    // so a dose that no longer opens -- because somebody ruined its payload --
    // goes with its medication all the same.
    use karmayoga_core::health::{Dose, DoseOrigin, DoseStatus};
    use karmayoga_core::store::health::HealthStore;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path(), true)).unwrap();
    let medication = a_health_medication("A medication");
    let now = jiff::Timestamp::from_second(1_800_000_000).unwrap();
    store.put_medication(&medication).unwrap();
    let dose = Dose {
        id: karmayoga_core::id::DoseId::new(),
        medication_id: medication.id,
        scheduled_at: None,
        at: now,
        status: DoseStatus::Taken,
        amount: None,
        origin: DoseOrigin::Manual,
        note: String::new(),
        key: String::new(),
        hidden: false,
        created_at: now,
        updated_at: now,
    };
    store.put_doses(std::slice::from_ref(&dose)).unwrap();
    store.flush().unwrap();

    raw(dir.path()).execute("UPDATE health_doses SET data = x'00'", []).unwrap();
    store.delete_medication(medication.id).unwrap();

    let count: i64 =
        raw(dir.path()).query_row("SELECT COUNT(*) FROM health_doses", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0, "the dose went with its medication without being opened");
}

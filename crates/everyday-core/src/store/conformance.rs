//! A shared conformance suite that every [`JournalStore`] implementation
//! must pass.
//!
//! The whole point of the storage abstraction is that the application cannot
//! tell which backend it is talking to. That guarantee is only real if it is
//! tested, so backends do not write their own CRUD tests — they call
//! [`run_all`] and inherit these.
//!
//! ```ignore
//! #[test]
//! fn passes_the_shared_conformance_suite() {
//!     let dir = tempfile::tempdir().unwrap();
//!     let store = MyStore::open(ctx_for(dir.path())).unwrap();
//!     everyday_core::store::conformance::run_all(store.as_ref());
//! }
//! ```

use super::agent::{AgentStore, ConversationQuery};
use super::calendars::{CalendarStore, EventQuery};
use super::library::{ItemQuery, ItemSort, LibraryStore, LogQuery};
use super::purpose::{GoalQuery, PurposeWindow};
use super::tasks::{BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore};
use super::trackers::ReadingQuery;
use super::{EntryQuery, JournalStore, SortOrder};
use crate::agent::{AgentSettings, Conversation, Memory, Message, Role, ToolCall};
use crate::calendar::{Calendar, Event, EventStatus};
use crate::id::{
    BlobId, BlockId, CalendarId, ConversationId, EntryId, EventId, GoalId, ItemId, JournalId,
    KindId, LogId, MemoryId, NoteId, ProjectId, ReadingId, RoleId, RoutineId, RoutineRunId, TaskId,
    TrackerId,
};
use crate::library::{
    ExternalRating, FieldDef, FieldType, Item, ItemStatus, Kind, Link, LogEntry, LogEvent,
    Progress, Verbs,
};
use crate::model::{Attachment, Entry, Journal, Location, MediaKind};
use crate::note::Note;
use crate::profile::Profile;
use crate::purpose::{Goal, GoalStatus, Purpose, Role as LifeRole};
use crate::richtext::{MEDIA_NODE, RichDoc};
use crate::routine::{Outcome, Routine, RoutineRun, Trigger};
use crate::store::notes::{NoteQuery, NoteSort};
use crate::store::routines::RunQuery;
use crate::task::{
    BlockKind, BlockSubject, Priority, Project, ProjectStatus, ProjectTaskCount, Task, TaskStatus,
    TimeBlock,
};
use crate::tracker::{Aggregate, Cadence, Period, Reading, Tracker, TrackerKind};
use jiff::Timestamp;
use jiff::civil::{Date, date, time};
use serde_json::json;

/// Run the entire suite. Panics with a descriptive message on first failure.
///
/// The store must be empty on entry; it is left empty on success.
pub fn run_all(store: &dyn JournalStore) {
    let name = store.backend();
    eprintln!("--- conformance suite for backend {name:?} ---");

    starts_empty(store);
    journal_crud(store);
    journal_get_missing_is_not_found(store);
    entry_round_trips_every_field(store);
    entry_put_is_idempotent(store);
    a_conditional_put_refuses_a_stale_write(store);
    entry_get_missing_is_not_found(store);
    list_entries_filters_and_sorts(store);
    list_entries_paginates(store);
    blobs_are_content_addressed_and_deduplicated(store);
    blob_ranges_match_the_full_payload(store);
    blob_get_missing_is_not_found(store);
    large_blob_round_trips(store);
    deleting_a_journal_removes_its_entries(store);
    garbage_collection_keeps_referenced_blobs(store);
    stats_reflect_contents(store);
    unicode_survives_a_round_trip(store);
    the_owner_round_trips_and_starts_empty(store);

    // The task domain is optional. A backend that has one must implement all
    // of it, so this is run whenever `tasks()` answers, and skipped -- with a
    // line saying so, since a silently skipped suite is worse than none --
    // when it does not.
    match store.tasks() {
        Some(tasks) => run_task_suite(tasks),
        None => eprintln!("backend {name:?} stores no tasks; skipping the task suite"),
    }

    // And the calendar domain, on the same terms.
    match store.calendars() {
        Some(calendars) => run_calendar_suite(calendars),
        None => eprintln!("backend {name:?} stores no calendars; skipping the calendar suite"),
    }

    // And the library, on the same terms again.
    match store.library() {
        Some(library) => {
            run_library_suite(library);
            // Not part of the library suite, because it is a question about
            // the *journal* store: garbage collection walks every record that
            // can hold a blob, and an item's cover is one of them.
            garbage_collection_keeps_library_covers(store);
        }
        None => eprintln!("backend {name:?} stores no library; skipping the library suite"),
    }

    // And the tracking domain, on the same terms again. It is handed the
    // whole journal store because two of the things worth checking are
    // cascades from *outside* the domain: what a deleted journal does to the
    // readings ticked in it, and what a deleted entry does to the ones
    // logged beside it.
    match store.trackers() {
        Some(_) => run_tracker_suite(store),
        None => eprintln!("backend {name:?} stores no trackers; skipping the tracking suite"),
    }

    // And the purpose domain, on the same terms again. It is handed the
    // whole journal store rather than just its own trait, because the
    // question it exists to answer -- where did the week go -- is a join
    // across the task, calendar and tracking tables, and a suite that only
    // saw roles and goals could not tell whether any of that works.
    match store.purpose() {
        Some(_) => run_purpose_suite(store),
        None => eprintln!("backend {name:?} stores no goals; skipping the purpose suite"),
    }

    // And notes, on the same terms again. Handed the whole journal store
    // because two of the things worth checking reach outside the domain: a
    // note's purpose pointer, and whether garbage collection knows that a
    // photograph dropped into a note is a photograph somebody wants kept.
    match store.notes() {
        Some(_) => run_note_suite(store),
        None => eprintln!("backend {name:?} stores no notes; skipping the note suite"),
    }

    // And the assistant's standing work. Handed the whole journal store,
    // because the cascade worth checking reaches out of the domain: deleting a
    // routine has to take the transcripts of its runs, and those live in the
    // agent store.
    match store.routines() {
        Some(_) => run_routine_suite(store),
        None => eprintln!("backend {name:?} stores no routines; skipping the routine suite"),
    }

    // And the assistant, on the same terms again.
    match store.agent() {
        Some(agent) => run_agent_suite(agent),
        None => eprintln!("backend {name:?} stores no assistant; skipping the agent suite"),
    }

    cleanup(store);
    eprintln!("--- backend {name:?} passed ---");
}

/// The assistant half of the suite. Called by [`run_all`] when the backend
/// has an [`AgentStore`]; public so a backend under construction can run it
/// alone.
///
/// The store must be empty of conversations, memories and agent settings on
/// entry; it is left empty on success.
pub fn run_agent_suite(store: &dyn AgentStore) {
    eprintln!("--- agent conformance suite ---");

    agent_starts_unconfigured(store);
    settings_round_trip(store);
    the_key_is_stored_apart_from_the_settings(store);
    conversation_crud(store);
    missing_agent_records_are_not_found(store);
    messages_come_back_in_the_order_they_were_written(store);
    a_streamed_message_can_be_rewritten_in_place(store);
    deleting_a_conversation_takes_its_messages(store);
    listing_conversations_is_newest_first_and_pages(store);
    memory_round_trips_and_outlives_its_conversation(store);
    unicode_survives_an_agent_round_trip(store);

    agent_cleanup(store);
    eprintln!("--- agent suite passed ---");
}

fn agent_cleanup(store: &dyn AgentStore) {
    for c in store.list_conversations(&ConversationQuery::default()).unwrap() {
        store.delete_conversation(c.id).unwrap();
    }
    for m in store.list_memories().unwrap() {
        store.delete_memory(m.id).unwrap();
    }
    store.delete_secret().unwrap();
    store.put_settings(&AgentSettings::default()).unwrap();
}

fn agent_starts_unconfigured(store: &dyn AgentStore) {
    let s = store.settings().unwrap();
    assert!(!s.enabled, "a backend with no settings record must report the assistant off");
    assert!(!s.has_key, "no key has been stored");
    assert!(!store.has_secret().unwrap());
    assert!(store.secret().unwrap().is_none());
    assert!(store.list_conversations(&ConversationQuery::default()).unwrap().is_empty());
    assert!(store.list_memories().unwrap().is_empty());
}

fn settings_round_trip(store: &dyn AgentStore) {
    let mut s = AgentSettings {
        enabled: true,
        instructions: "Be terse. Never use exclamation marks.".into(),
        confirm_destructive: false,
        max_steps: 12,
        remember: false,
        ..Default::default()
    };
    s.model.model = "gpt-5.1".into();
    s.model.base_url = Some("https://gateway.example.com/v1".into());
    s.model.temperature = Some(0.3);
    s.model.max_tokens = Some(2048);

    store.put_settings(&s).unwrap();
    let back = store.settings().unwrap();
    assert_eq!(back.enabled, s.enabled);
    assert_eq!(back.instructions, s.instructions);
    assert_eq!(back.confirm_destructive, s.confirm_destructive);
    assert_eq!(back.max_steps, s.max_steps);
    assert_eq!(back.remember, s.remember);
    assert_eq!(back.model, s.model);

    // Saving twice must update rather than accumulate.
    store.put_settings(&s).unwrap();
    assert_eq!(store.settings().unwrap().model.model, "gpt-5.1");

    store.put_settings(&AgentSettings::default()).unwrap();
    assert!(!store.settings().unwrap().enabled, "settings must be replaceable, not merged");
}

/// The credential must not be reachable through the settings, and
/// `has_key` must be derived from the secret rather than from anything a
/// caller wrote into the settings record.
fn the_key_is_stored_apart_from_the_settings(store: &dyn AgentStore) {
    // A caller lying about `has_key` must not be believed.
    store.put_settings(&AgentSettings { has_key: true, ..Default::default() }).unwrap();
    assert!(!store.settings().unwrap().has_key, "has_key must come from the secret table");

    store.put_secret("sk-test-0123456789").unwrap();
    assert!(store.has_secret().unwrap());
    assert!(store.settings().unwrap().has_key, "storing a key must show in the settings");
    assert_eq!(store.secret().unwrap().as_deref(), Some("sk-test-0123456789"));

    // Replacing, not accumulating.
    store.put_secret("sk-test-second").unwrap();
    assert_eq!(store.secret().unwrap().as_deref(), Some("sk-test-second"));

    // Surrounding whitespace is what a paste from a web page carries, and a
    // key with a newline on the end fails at the endpoint with a message
    // nobody can act on.
    store.put_secret("  sk-test-padded\n").unwrap();
    assert_eq!(store.secret().unwrap().as_deref(), Some("sk-test-padded"));

    assert!(store.put_secret("   ").is_err(), "a blank key is a missing key, not a stored one");

    store.delete_secret().unwrap();
    assert!(!store.has_secret().unwrap());
    assert!(store.secret().unwrap().is_none());
    assert!(!store.settings().unwrap().has_key);

    // Deleting a key that is not there is not an error: the settings pane
    // clears the field whether or not one was set.
    store.delete_secret().unwrap();

    store.put_settings(&AgentSettings::default()).unwrap();
}

fn seeded_conversation(store: &dyn AgentStore, title: &str) -> Conversation {
    let c = Conversation { title: title.into(), ..Conversation::new() };
    store.put_conversation(&c).unwrap();
    c
}

fn conversation_crud(store: &dyn AgentStore) {
    let mut c = seeded_conversation(store, "Plan my week");
    assert_eq!(store.get_conversation(c.id).unwrap().title, "Plan my week");
    assert_eq!(store.count_messages(c.id).unwrap(), 0);

    c.title = "Plan my fortnight".into();
    c.updated_at = Timestamp::now();
    store.put_conversation(&c).unwrap();
    let back = store.get_conversation(c.id).unwrap();
    assert_eq!(back.title, "Plan my fortnight");
    assert_eq!(back.created_at, c.created_at, "a rename must not restart the clock");

    store.delete_conversation(c.id).unwrap();
    assert!(store.get_conversation(c.id).is_err());
}

fn missing_agent_records_are_not_found(store: &dyn AgentStore) {
    let e = store.get_conversation(ConversationId::new()).unwrap_err();
    assert_eq!(e.code(), "not_found", "got {e:?}");
    // Deleting what is not there is a no-op everywhere else in this crate,
    // and must be here too: two panels racing to clear the same thread.
    store.delete_conversation(ConversationId::new()).unwrap();
    store.delete_memory(MemoryId::new()).unwrap();
    // A thread that does not exist has no messages rather than an error.
    assert!(store.list_messages(ConversationId::new()).unwrap().is_empty());
    assert_eq!(store.count_messages(ConversationId::new()).unwrap(), 0);
}

/// Order is the contract. A model handed its own tool calls out of order
/// re-runs them, so this is the test that stops a resumed conversation
/// silently adding the same task twice.
fn messages_come_back_in_the_order_they_were_written(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Ordering");

    let call = ToolCall {
        id: "call_1".into(),
        name: "add_task".into(),
        arguments: json!({ "title": "Ring the vet" }),
    };
    let ask = Message::user(c.id, "add a task to ring the vet");
    let plan = Message::assistant(c.id, String::new()).with_tool_calls(vec![call.clone()]);
    let result = Message::tool_result(c.id, &call, Ok("added task 3f2a".into()));
    let reply = Message::assistant(c.id, "Added it.");

    // Written in order, but with timestamps that collide -- which is what
    // actually happens, since a tool call and its result land in the same
    // microsecond on any machine fast enough to run one locally.
    let stamp = Timestamp::now();
    for m in [&ask, &plan, &result, &reply] {
        store.append_message(&Message { created_at: stamp, ..m.clone() }).unwrap();
    }

    let back = store.list_messages(c.id).unwrap();
    assert_eq!(back.len(), 4);
    assert_eq!(
        back.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![ask.id, plan.id, result.id, reply.id],
        "turns must replay in the order they were written"
    );
    assert_eq!(back[0].role, Role::User);
    assert_eq!(back[1].tool_calls, vec![call.clone()], "tool calls must survive the round trip");
    assert_eq!(back[2].role, Role::Tool);
    assert_eq!(back[2].tool_call_id.as_deref(), Some("call_1"));
    assert!(!back[2].failed);
    assert_eq!(store.count_messages(c.id).unwrap(), 4);

    // A failed call is still sent to the model, and must still be drawable
    // as a failure without parsing its prose.
    let failure = Message::tool_result(c.id, &call, Err("no project by that name".into()));
    store.append_message(&failure).unwrap();
    let back = store.list_messages(c.id).unwrap();
    assert!(back.last().unwrap().failed);

    store.delete_message(failure.id).unwrap();
    assert_eq!(store.count_messages(c.id).unwrap(), 4);

    store.delete_conversation(c.id).unwrap();
}

/// An assistant turn is written empty when the stream opens and rewritten
/// when it closes. Without this a cancelled stream leaves a thread with no
/// record that the model ever replied.
fn a_streamed_message_can_be_rewritten_in_place(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Streaming");

    let mut m = Message::assistant(c.id, String::new());
    store.append_message(&m).unwrap();
    assert_eq!(store.count_messages(c.id).unwrap(), 1);

    m.content = "Here is what I found.".into();
    m.tool_calls = vec![ToolCall {
        id: "call_2".into(),
        name: "list_tasks".into(),
        arguments: json!({ "status": "todo" }),
    }];
    store.put_message(&m).unwrap();

    let back = store.list_messages(c.id).unwrap();
    assert_eq!(back.len(), 1, "rewriting a message must not append a second one");
    assert_eq!(back[0].content, "Here is what I found.");
    assert_eq!(back[0].tool_calls.len(), 1);

    store.delete_conversation(c.id).unwrap();
}

fn deleting_a_conversation_takes_its_messages(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Doomed");
    let m = Message::user(c.id, "hello");
    store.append_message(&m).unwrap();

    let other = seeded_conversation(store, "Spared");
    store.append_message(&Message::user(other.id, "still here")).unwrap();

    store.delete_conversation(c.id).unwrap();
    assert!(store.list_messages(c.id).unwrap().is_empty());
    assert_eq!(store.count_messages(c.id).unwrap(), 0);
    assert_eq!(store.count_messages(other.id).unwrap(), 1, "the other thread must be untouched");

    store.delete_conversation(other.id).unwrap();
}

fn listing_conversations_is_newest_first_and_pages(store: &dyn AgentStore) {
    let base = Timestamp::now();
    let mut made = Vec::new();
    for i in 0..5i64 {
        let c = Conversation {
            title: format!("thread {i}"),
            // Distinct, ascending, so "newest first" has one right answer.
            updated_at: base + jiff::SignedDuration::from_secs(i),
            ..Conversation::new()
        };
        store.put_conversation(&c).unwrap();
        made.push(c);
    }

    let all = store.list_conversations(&ConversationQuery::default()).unwrap();
    assert_eq!(all.len(), 5, "an empty query returns every thread");
    assert_eq!(
        all.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["thread 4", "thread 3", "thread 2", "thread 1", "thread 0"],
        "most recently used first"
    );

    let page = store.list_conversations(&ConversationQuery::recent(2)).unwrap();
    assert_eq!(
        page.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["thread 4", "thread 3"]
    );

    let second = store
        .list_conversations(&ConversationQuery { limit: Some(2), offset: 2, chats_only: false })
        .unwrap();
    assert_eq!(
        second.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["thread 2", "thread 1"]
    );

    // An offset with no limit is a real query, not a no-op.
    let tail = store
        .list_conversations(&ConversationQuery { limit: None, offset: 3, chats_only: false })
        .unwrap();
    assert_eq!(tail.len(), 2);

    // Past the end is empty rather than a panic.
    assert!(
        store
            .list_conversations(&ConversationQuery {
                limit: Some(2),
                offset: 99,
                chats_only: false
            })
            .unwrap()
            .is_empty()
    );

    for c in made {
        store.delete_conversation(c.id).unwrap();
    }
}

/// Clearing your chat history must not retract what it taught.
fn memory_round_trips_and_outlives_its_conversation(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Where a memory came from");

    let mut m = Memory::from_conversation("Plans the week on Sunday evening", c.id);
    store.put_memory(&m).unwrap();

    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].text, "Plans the week on Sunday evening");
    assert_eq!(back[0].source_id, Some(c.id));
    assert!(!back[0].pinned);

    // Editing in place rather than appending a second copy.
    m.text = "Plans the week on Sunday afternoon".into();
    m.pinned = true;
    m.updated_at = Timestamp::now();
    store.put_memory(&m).unwrap();
    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].text, "Plans the week on Sunday afternoon");
    assert!(back[0].pinned, "a hand-edited memory must stay protected from housekeeping");

    // The whole point: the thread goes, the fact stays.
    store.delete_conversation(c.id).unwrap();
    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 1, "a memory must outlive the conversation that produced it");
    assert_eq!(back[0].source_id, Some(c.id), "the dangling pointer is kept on purpose");

    // Oldest first, because that is the order they are read into a prompt.
    let second = Memory::new("Calls the deck project 'the deck'");
    store.put_memory(&second).unwrap();
    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].id, m.id, "memories are listed oldest first");
    assert_eq!(back[1].id, second.id);

    store.delete_memory(m.id).unwrap();
    store.delete_memory(second.id).unwrap();
    assert!(store.list_memories().unwrap().is_empty());
}

fn unicode_survives_an_agent_round_trip(store: &dyn AgentStore) {
    let title = "\u{5468}\u{672b}\u{8ba1}\u{5212} \u{1f5d3}\u{fe0f} caf\u{e9}";
    let c = seeded_conversation(store, title);
    assert_eq!(store.get_conversation(c.id).unwrap().title, title);

    let body = "R\u{e9}sum\u{e9}: \u{4f60}\u{597d} \u{1f469}\u{200d}\u{1f4bb} \u{2014} na\u{ef}ve";
    store.append_message(&Message::user(c.id, body)).unwrap();
    assert_eq!(store.list_messages(c.id).unwrap()[0].content, body);

    let m = Memory::new(body);
    store.put_memory(&m).unwrap();
    assert_eq!(store.list_memories().unwrap()[0].text, body);

    store.delete_memory(m.id).unwrap();
    store.delete_conversation(c.id).unwrap();
}

/// The task half of the suite. Called by [`run_all`] when the backend has a
/// [`TaskStore`]; public so a backend under construction can run it alone.
///
/// The store must be empty of tasks on entry; it is left empty on success.
pub fn run_task_suite(store: &dyn TaskStore) {
    eprintln!("--- task conformance suite ---");

    tasks_start_empty(store);
    project_crud(store);
    task_round_trips_every_field(store);
    task_put_is_idempotent(store);
    missing_task_records_are_not_found(store);
    list_tasks_filters_and_sorts(store);
    list_tasks_paginates(store);
    batch_writes_land_together(store);
    subtasks_are_reachable_from_their_parent(store);
    deleting_a_task_takes_its_subtrees_with_it(store);
    deleting_a_project_takes_its_tasks_with_it(store);
    time_blocks_round_trip_and_query_by_day(store);
    deleting_a_task_removes_its_time_blocks(store);
    task_stats_reflect_contents(store);
    unicode_survives_a_task_round_trip(store);

    task_cleanup(store);
    eprintln!("--- task suite passed ---");
}

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

/// The library half of the suite. Called by [`run_all`] when the backend has
/// a [`LibraryStore`]; public so a backend under construction can run it
/// alone.
///
/// The store must be empty of kinds on entry; it is left empty on success.
pub fn run_library_suite(store: &dyn LibraryStore) {
    eprintln!("--- library conformance suite ---");

    library_starts_empty(store);
    kind_crud(store);
    missing_library_records_are_not_found(store);
    item_round_trips_every_field(store);
    batch_item_writes_land_together(store);
    listing_items_filters_and_sorts(store);
    listing_items_paginates(store);
    shelf_counts_are_answered_without_decrypting_everything(store);
    the_log_round_trips_and_queries_by_window(store);
    deleting_an_item_takes_its_log_with_it(store);
    deleting_a_kind_takes_its_shelf_with_it(store);
    unicode_survives_a_library_round_trip(store);

    library_cleanup(store);
    eprintln!("--- library suite passed ---");
}

/// A cover must survive the garbage collector.
///
/// The bug this exists to prevent is the quietest kind there is. Liveness is
/// decided by walking the records that reference a blob, and until the
/// library was added that walk knew only about entries. Missing it here
/// meant the first `everyday gc` after a day's grace deleted the cover of
/// every book on every shelf -- with no error anywhere, the items still
/// present, and each one holding a blob id pointing at nothing.
fn garbage_collection_keeps_library_covers(store: &dyn JournalStore) {
    let Some(library) = store.library() else { return };

    let kind = seeded_kind(library, "gc");
    let cover = store.put_blob(b"a book jacket").unwrap();
    let orphan = store.put_blob(b"nobody's cover").unwrap();

    let mut item = Item::new(kind.id, "Dune");
    item.cover = Some(cover);
    library.put_item(&item).unwrap();

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert_eq!(removed, 1, "exactly the unreferenced blob should be collected");
    assert!(store.has_blob(cover).unwrap(), "a cover on a shelf is a live reference");
    assert!(!store.has_blob(orphan).unwrap());

    // ...and it stops being one when the item goes.
    library.delete_item(item.id).unwrap();
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 1);
    assert!(!store.has_blob(cover).unwrap(), "a cover nothing points at is collectable");

    library.delete_kind(kind.id).unwrap();
}

fn library_cleanup(store: &dyn LibraryStore) {
    for kind in store.list_kinds().expect("list_kinds") {
        store.delete_kind(kind.id).expect("delete_kind");
    }
    assert!(store.list_kinds().unwrap().is_empty(), "cleanup left kinds behind");
    assert!(
        store.list_items(&ItemQuery::default()).unwrap().is_empty(),
        "cleanup left items behind"
    );
    assert!(store.list_logs(&LogQuery::default()).unwrap().is_empty(), "cleanup left logs behind");
}

fn seeded_kind(store: &dyn LibraryStore, slug: &str) -> Kind {
    let kind = Kind::new(slug, "Books", "Book")
        .with_verbs(Verbs::new("To read", "Reading", "Read", "read"))
        .with_fields(vec![
            FieldDef::new("author", "Author", FieldType::Text),
            FieldDef::new("pages", "Pages", FieldType::Number),
        ]);
    store.put_kind(&kind).expect("put_kind");
    kind
}

fn seeded_item(store: &dyn LibraryStore, kind: KindId, title: &str) -> Item {
    let item = Item::new(kind, title);
    store.put_item(&item).expect("put_item");
    item
}

fn library_starts_empty(store: &dyn LibraryStore) {
    assert!(store.list_kinds().unwrap().is_empty(), "a fresh store must have no kinds");
    assert!(
        store.list_items(&ItemQuery::default()).unwrap().is_empty(),
        "a fresh store must have no items"
    );
    assert!(store.list_logs(&LogQuery::default()).unwrap().is_empty(), "a fresh store has no log");
}

fn kind_crud(store: &dyn LibraryStore) {
    let mut kind = seeded_kind(store, "crud");
    kind.icon = "\u{1f4d9}".into();
    kind.color = "#0f766e".into();
    kind.progress_unit = "page".into();
    kind.source = "openLibrary".into();
    kind.sort_order = 3;
    kind.visible = false;
    kind.builtin = true;
    store.put_kind(&kind).expect("put_kind");

    let back = store.get_kind(kind.id).expect("get_kind");
    assert_eq!(back, kind, "a kind must survive the round trip unchanged");
    assert_eq!(back.fields.len(), 2, "a kind's field definitions must persist");
    assert_eq!(back.verbs.active, "Reading");
    assert_eq!(store.list_kinds().unwrap().len(), 1);

    // Idempotent: writing the same id twice updates rather than duplicates.
    kind.name = "Books (renamed)".into();
    store.put_kind(&kind).expect("put_kind again");
    assert_eq!(store.list_kinds().unwrap().len(), 1, "put must not duplicate");
    assert_eq!(store.get_kind(kind.id).unwrap().name, "Books (renamed)");

    store.delete_kind(kind.id).expect("delete_kind");
    assert!(store.list_kinds().unwrap().is_empty());
}

fn missing_library_records_are_not_found(store: &dyn LibraryStore) {
    for err in [
        store.get_kind(KindId::new()).unwrap_err(),
        store.get_item(ItemId::new()).unwrap_err(),
        store.get_log(LogId::new()).unwrap_err(),
    ] {
        assert!(matches!(err, crate::Error::NotFound { .. }), "expected NotFound, got {err:?}");
    }
    assert_eq!(
        store.count_items(KindId::new()).expect("count_items"),
        (0, 0),
        "a shelf that does not exist holds nothing, and asking is not an error",
    );
}

fn item_round_trips_every_field(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "round-trip");
    let mut item = Item::new(kind.id, "Dune");
    item.subtitle = "Book one".into();
    item.creator = "Frank Herbert".into();
    item.year = Some(1965);
    item.status = ItemStatus::Done;
    item.rating = Some(90);
    item.external = vec![ExternalRating {
        source: "Open Library".into(),
        score: 84,
        count: Some(1204),
        url: "https://openlibrary.org/works/OL893415W".into(),
    }];
    item.cover = Some(BlobId::of(b"a cover"));
    item.cover_url = "https://covers.openlibrary.org/b/id/1.jpg".into();
    item.summary = "A desert planet.".into();
    item.notes = "Lent to Ana.".into();
    item.tags = vec!["sci-fi".into(), "reread".into()];
    item.facts.insert("author".into(), "Frank Herbert".into());
    item.facts.insert("pages".into(), "412".into());
    item.links = vec![Link { label: "Open Library".into(), url: "https://ol.example".into() }];
    item.progress = Some(Progress::new(412, Some(412), "page"));
    item.favourite = true;
    item.started_on = Some(date(2026, 3, 3));
    item.finished_on = Some(date(2026, 4, 2));
    item.source = "openLibrary".into();
    item.sort_order = 7;

    store.put_item(&item).expect("put_item");
    let back = store.get_item(item.id).expect("get_item");
    assert_eq!(back, item, "an item must survive the round trip unchanged");

    // Idempotent, like every other `put` in this crate.
    store.put_item(&item).expect("put_item again");
    assert_eq!(store.list_items(&ItemQuery::on_shelf(kind.id)).unwrap().len(), 1);

    store.delete_kind(kind.id).expect("delete_kind");
}

fn batch_item_writes_land_together(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "batch");
    let mut items: Vec<Item> = ["a", "b", "c"].iter().map(|t| Item::new(kind.id, *t)).collect();
    store.put_items(&items).expect("put_items");
    assert_eq!(store.list_items(&ItemQuery::on_shelf(kind.id)).unwrap().len(), 3);

    // What a re-ordered shelf is: one write for many rows.
    for (i, item) in items.iter_mut().enumerate() {
        item.sort_order = 10 - i as i32;
    }
    store.put_items(&items).expect("put_items again");
    let back = store
        .list_items(&ItemQuery { sort: ItemSort::Manual, ..ItemQuery::on_shelf(kind.id) })
        .unwrap();
    assert_eq!(
        back.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        ["c", "b", "a"],
        "a batch write must land in full",
    );
    // An empty batch is a no-op rather than an error.
    store.put_items(&[]).expect("put_items with none");

    store.delete_kind(kind.id).expect("delete_kind");
}

fn listing_items_filters_and_sorts(store: &dyn LibraryStore) {
    let books = seeded_kind(store, "filter-books");
    let films = seeded_kind(store, "filter-films");

    let mut dune = Item::new(books.id, "Dune");
    dune.creator = "Frank Herbert".into();
    dune.status = ItemStatus::Done;
    dune.rating = Some(90);
    dune.tags = vec!["Sci-Fi".into()];
    dune.finished_on = Some(date(2026, 4, 2));
    dune.year = Some(1965);

    let mut pnin = Item::new(books.id, "Pnin");
    pnin.creator = "Vladimir Nabokov".into();
    pnin.status = ItemStatus::Wishlist;
    pnin.year = Some(1957);

    let mut arrival = Item::new(films.id, "Arrival");
    arrival.status = ItemStatus::Done;
    arrival.rating = Some(95);
    arrival.finished_on = Some(date(2025, 11, 1));

    store.put_items(&[dune.clone(), pnin.clone(), arrival.clone()]).expect("put_items");

    let on_shelf = store.list_items(&ItemQuery::on_shelf(books.id)).unwrap();
    assert_eq!(on_shelf.len(), 2, "the shelf filter must exclude other kinds");

    let done = store
        .list_items(&ItemQuery { statuses: vec![ItemStatus::Done], ..Default::default() })
        .unwrap();
    assert_eq!(done.len(), 2, "the status filter spans every shelf when no kind is given");

    // Text search reaches the creator, and ignores case.
    let by_author =
        store.list_items(&ItemQuery { text: "NABOKOV".into(), ..Default::default() }).unwrap();
    assert_eq!(by_author.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["Pnin"]);

    // Tags, likewise.
    let tagged =
        store.list_items(&ItemQuery { tags: vec!["sci-fi".into()], ..Default::default() }).unwrap();
    assert_eq!(tagged.len(), 1);

    // An unrated item must not pass a lower bound on the rating.
    let good =
        store.list_items(&ItemQuery { rating_at_least: Some(80), ..Default::default() }).unwrap();
    assert_eq!(good.len(), 2, "an unrated item is not a zero-rated one");

    // The year-in-review window.
    let this_year = store
        .list_items(&ItemQuery {
            finished_from: Some(date(2026, 1, 1)),
            finished_to: Some(date(2026, 12, 31)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(this_year.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["Dune"]);

    // Sorting: best first, with the unrated last rather than first.
    let by_rating =
        store.list_items(&ItemQuery { sort: ItemSort::RatingDesc, ..Default::default() }).unwrap();
    assert_eq!(
        by_rating.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        ["Arrival", "Dune", "Pnin"],
    );
    let by_year =
        store.list_items(&ItemQuery { sort: ItemSort::YearAsc, ..Default::default() }).unwrap();
    assert_eq!(by_year[0].title, "Pnin", "1957 comes before 1965");
    assert_eq!(by_year[2].title, "Arrival", "an unknown year sorts last");

    store.delete_kind(books.id).expect("delete_kind");
    store.delete_kind(films.id).expect("delete_kind");
}

fn listing_items_paginates(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "paginate");
    let items: Vec<Item> =
        ["e", "d", "c", "b", "a"].iter().map(|t| Item::new(kind.id, *t)).collect();
    store.put_items(&items).expect("put_items");

    let page = store
        .list_items(&ItemQuery {
            sort: ItemSort::TitleAsc,
            offset: 1,
            limit: Some(2),
            ..ItemQuery::on_shelf(kind.id)
        })
        .unwrap();
    assert_eq!(page.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["b", "c"]);

    let past_the_end =
        store.list_items(&ItemQuery { offset: 99, ..ItemQuery::on_shelf(kind.id) }).unwrap();
    assert!(past_the_end.is_empty(), "paging past the end yields nothing, not an error");

    store.delete_kind(kind.id).expect("delete_kind");
}

fn shelf_counts_are_answered_without_decrypting_everything(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "counts");
    let mut items = Vec::new();
    for status in ItemStatus::ALL {
        let mut item = Item::new(kind.id, status.as_str());
        item.status = status;
        items.push(item);
    }
    store.put_items(&items).expect("put_items");

    // Five statuses, three of which are still ahead of you.
    assert_eq!(store.count_items(kind.id).expect("count_items"), (5, 3));

    store.delete_kind(kind.id).expect("delete_kind");
}

fn the_log_round_trips_and_queries_by_window(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "log");
    let dune = seeded_item(store, kind.id, "Dune");
    let pnin = seeded_item(store, kind.id, "Pnin");

    let mut started = LogEntry::new(dune.id, LogEvent::Started, date(2026, 3, 3), "Europe/London");
    started.note = "Picked it up again.".into();
    let mut finished = LogEntry::new(dune.id, LogEvent::Finished, date(2026, 4, 2), "UTC");
    finished.rating = Some(90);
    finished.minutes = Some(640);
    finished.position = Some(412);
    let reread = LogEntry::new(dune.id, LogEvent::Revisited, date(2026, 9, 1), "UTC");
    let other = LogEntry::new(pnin.id, LogEvent::Finished, date(2025, 12, 30), "UTC");
    for log in [&started, &finished, &reread, &other] {
        store.put_log(log).expect("put_log");
    }

    assert_eq!(store.get_log(finished.id).expect("get_log"), finished, "a log must round trip");

    // One item's history, newest first.
    let history = store.list_logs(&LogQuery::for_item(dune.id)).expect("list_logs");
    assert_eq!(
        history.iter().map(|l| l.date).collect::<Vec<_>>(),
        [date(2026, 9, 1), date(2026, 4, 2), date(2026, 3, 3)],
        "a history reads newest first",
    );

    // The year in review: both ways of getting to the end of something, and
    // nothing else.
    let year = store
        .list_logs(&LogQuery::completions(date(2026, 1, 1), date(2026, 12, 31)))
        .expect("list_logs");
    assert_eq!(year.len(), 2, "started is not a completion, and last year is not this one");

    // Idempotent, and a limit is honoured.
    store.put_log(&finished).expect("put_log again");
    let capped = store
        .list_logs(&LogQuery { limit: Some(1), ..LogQuery::for_item(dune.id) })
        .expect("list_logs");
    assert_eq!(capped.len(), 1);

    store.delete_log(started.id).expect("delete_log");
    assert_eq!(store.list_logs(&LogQuery::for_item(dune.id)).unwrap().len(), 2);

    store.delete_kind(kind.id).expect("delete_kind");
}

fn deleting_an_item_takes_its_log_with_it(store: &dyn LibraryStore) {
    // The bug this guards: a log row surviving its item, so the year in
    // review counts a book that is no longer in the vault and cannot be
    // named.
    let kind = seeded_kind(store, "cascade-item");
    let dune = seeded_item(store, kind.id, "Dune");
    let pnin = seeded_item(store, kind.id, "Pnin");
    store
        .put_log(&LogEntry::new(dune.id, LogEvent::Finished, date(2026, 4, 2), "UTC"))
        .expect("put_log");
    let keep = LogEntry::new(pnin.id, LogEvent::Finished, date(2026, 5, 1), "UTC");
    store.put_log(&keep).expect("put_log");

    store.delete_item(dune.id).expect("delete_item");
    assert!(store.get_item(dune.id).is_err(), "the item should be gone");
    assert!(
        store.list_logs(&LogQuery::for_item(dune.id)).unwrap().is_empty(),
        "an item's log must go with it",
    );
    assert_eq!(
        store.list_logs(&LogQuery::default()).unwrap().len(),
        1,
        "one item's deletion must not take another's log",
    );

    store.delete_kind(kind.id).expect("delete_kind");
}

fn deleting_a_kind_takes_its_shelf_with_it(store: &dyn LibraryStore) {
    let doomed = seeded_kind(store, "cascade-kind");
    let spared = seeded_kind(store, "cascade-spared");
    let dune = seeded_item(store, doomed.id, "Dune");
    let pnin = seeded_item(store, spared.id, "Pnin");
    store
        .put_log(&LogEntry::new(dune.id, LogEvent::Finished, date(2026, 4, 2), "UTC"))
        .expect("put_log");
    store
        .put_log(&LogEntry::new(pnin.id, LogEvent::Finished, date(2026, 5, 1), "UTC"))
        .expect("put_log");

    store.delete_kind(doomed.id).expect("delete_kind");
    assert!(store.get_item(dune.id).is_err(), "a shelf takes its items");
    assert!(
        store.list_logs(&LogQuery::for_item(dune.id)).unwrap().is_empty(),
        "a shelf takes its items' logs too",
    );
    assert!(store.get_item(pnin.id).is_ok(), "another shelf must be untouched");
    assert_eq!(store.list_logs(&LogQuery::default()).unwrap().len(), 1);

    store.delete_kind(spared.id).expect("delete_kind");
}

fn unicode_survives_a_library_round_trip(store: &dyn LibraryStore) {
    let mut kind = Kind::new("\u{6f22}\u{5b57}", "\u{6620}\u{753b}", "\u{6620}\u{753b}");
    kind.icon = "\u{1f3ac}".into();
    store.put_kind(&kind).expect("put_kind");

    let mut item = Item::new(kind.id, "\u{1f480} Se\u{00f1}or Babel \u{2014} \u{6f22}\u{5b57}");
    item.creator = "\u{00c1}sd\u{00ed}s \u{00d3}sk".into();
    item.notes =
        "emoji \u{1f4da}\u{1f3c1}, combining a\u{0301}, RTL \u{05e2}\u{05d1}\u{05e8}".into();
    item.facts.insert("author".into(), "\u{6751}\u{4e0a}\u{6625}\u{6a39}".into());
    store.put_item(&item).expect("put_item");

    let back = store.get_item(item.id).expect("get_item");
    assert_eq!(back, item, "unicode must survive sealing and storage");
    assert_eq!(store.get_kind(kind.id).unwrap().name, "\u{6620}\u{753b}");

    // ...and the text filter must find it, which is a different question.
    let found =
        store.list_items(&ItemQuery { text: "\u{6f22}\u{5b57}".into(), ..Default::default() });
    assert_eq!(found.unwrap().len(), 1, "a text filter must match non-ASCII");

    store.delete_kind(kind.id).expect("delete_kind");
}

fn cleanup(store: &dyn JournalStore) {
    for j in store.list_journals().expect("list_journals") {
        store.delete_journal(j.id).expect("delete_journal");
    }
    for b in store.list_blobs().expect("list_blobs") {
        store.delete_blob(b).expect("delete_blob");
    }
    assert!(store.list_journals().unwrap().is_empty(), "cleanup left journals behind");
    assert!(
        store.list_entries(&EntryQuery::default()).unwrap().is_empty(),
        "cleanup left entries behind"
    );
}

fn seeded_journal(store: &dyn JournalStore, name: &str) -> Journal {
    let j = Journal::new(name);
    store.put_journal(&j).expect("put_journal");
    j
}

fn seeded_entry(store: &dyn JournalStore, jid: JournalId, when: Date, text: &str) -> Entry {
    let mut e = Entry::new(jid, "UTC");
    e.local_date = when;
    e.body = RichDoc::from_plain_text(text);
    store.put_entry(&e).expect("put_entry");
    e
}

fn starts_empty(store: &dyn JournalStore) {
    assert!(store.list_journals().unwrap().is_empty(), "a fresh store must have no journals");
    assert!(
        store.list_entries(&EntryQuery::default()).unwrap().is_empty(),
        "a fresh store must have no entries"
    );
    assert_eq!(store.stats().unwrap().entries, 0);
}

fn journal_crud(store: &dyn JournalStore) {
    let mut j = Journal::new("Travel").with_color("#0f766e").with_icon("\u{2708}");
    j.description = "Trips and trains".into();
    j.sort_order = 3;
    store.put_journal(&j).unwrap();

    let got = store.get_journal(j.id).expect("journal should exist after put");
    assert_eq!(got, j, "journal must round-trip unchanged");

    j.name = "Travel & Trains".into();
    store.put_journal(&j).unwrap();
    assert_eq!(store.get_journal(j.id).unwrap().name, "Travel & Trains", "put must upsert");
    assert_eq!(store.list_journals().unwrap().len(), 1, "upsert must not duplicate");

    store.delete_journal(j.id).unwrap();
    assert!(store.get_journal(j.id).is_err(), "journal must be gone after delete");
    assert!(store.list_journals().unwrap().is_empty());
}

fn journal_get_missing_is_not_found(store: &dyn JournalStore) {
    let err = store.get_journal(JournalId::new()).unwrap_err();
    assert_eq!(err.code(), "not_found", "missing journal must report not_found, got {err}");
}

fn entry_round_trips_every_field(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Daily");
    let blob = store.put_blob(b"fake jpeg bytes").unwrap();

    let mut e = Entry::new(j.id, "Europe/Berlin");
    e.title = "A long walk".into();
    e.body = RichDoc(json!({
        "type": "doc",
        "content": [
            {"type": "heading", "attrs": {"level": 1},
             "content": [{"type": "text", "text": "A long walk"}]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "We went "},
                {"type": "text", "text": "far", "marks": [{"type": "bold"}]},
            ]},
            {"type": MEDIA_NODE, "attrs": {
                "blob": blob.to_hex(), "kind": "image", "mime": "image/jpeg",
                "filename": "walk.jpg", "caption": "the ridge"}},
        ]
    }));
    e.local_date = date(2024, 7, 14);
    e.tags = vec!["walking".into(), "summer".into()];
    e.starred = true;
    e.pinned = true;
    e.location = Some(Location {
        latitude: 52.52,
        longitude: 13.405,
        place_name: Some("Tempelhofer Feld".into()),
        locality: Some("Berlin".into()),
        country: Some("Germany".into()),
    });
    e.attachments = vec![Attachment {
        blob,
        kind: MediaKind::Image,
        mime: "image/jpeg".into(),
        filename: "walk.jpg".into(),
        byte_len: 15,
        width: Some(4032),
        height: Some(3024),
        duration_ms: None,
        caption: "the ridge".into(),
    }];
    store.put_entry(&e).unwrap();

    let got = store.get_entry(e.id).expect("entry should exist after put");
    assert_eq!(got.id, e.id);
    assert_eq!(got.journal_id, e.journal_id);
    assert_eq!(got.title, e.title);
    assert_eq!(got.body, e.body, "rich text body must round-trip byte-identically");
    assert_eq!(got.local_date, e.local_date);
    assert_eq!(got.tz, e.tz, "time zone must survive; it is how local dates are recomputed");
    assert_eq!(got.tags, e.tags);
    assert_eq!(got.starred, e.starred);
    assert_eq!(got.pinned, e.pinned);
    assert_eq!(got.location, e.location);
    assert_eq!(got.attachments, e.attachments);
    assert_eq!(got.created_at, e.created_at);

    // Summaries must be derived consistently with the full entry.
    let rows = store.list_entries(&EntryQuery::in_journal(j.id)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "A long walk");
    assert_eq!(rows[0].cover, Some(blob), "first image should become the list thumbnail");
    assert_eq!(rows[0].attachment_count, 1);

    // all_entries must return full bodies, not summaries.
    let all = store.all_entries().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].body, e.body);

    cleanup(store);
}

fn entry_put_is_idempotent(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Daily");
    let mut e = seeded_entry(store, j.id, date(2024, 1, 1), "first");

    e.body = RichDoc::from_plain_text("second");
    e.updated_at = jiff::Timestamp::now();
    store.put_entry(&e).unwrap();

    assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 1, "put must upsert");
    assert_eq!(store.get_entry(e.id).unwrap().body.plain_text(), "second");

    store.delete_entry(e.id).unwrap();
    assert!(store.get_entry(e.id).is_err());
    // Deleting twice is not an error worth crashing the UI over, but it must
    // not resurrect anything either.
    let _ = store.delete_entry(e.id);
    assert!(store.list_entries(&EntryQuery::default()).unwrap().is_empty());

    cleanup(store);
}

/// The optimistic-concurrency contract, which every backend must honour.
///
/// The scenario is two writers with the same entry open. Both loaded version
/// `v1`; one saves and the store moves to `v2`; the other saves believing it
/// is still replacing `v1`. Without the check the second write wins and the
/// first author's paragraph is gone with no error anywhere.
fn a_conditional_put_refuses_a_stale_write(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Contended");

    // A create: nothing should be there, and nothing is.
    let mut mine = Entry::new(j.id, "UTC");
    mine.body = RichDoc::from_plain_text("v1");
    store.put_entry_if(&mine, None).expect("creating a new entry must be allowed");

    // Creating the same id twice is a conflict, not an overwrite.
    let mut clash = mine.clone();
    clash.body = RichDoc::from_plain_text("also v1");
    assert_eq!(
        store.put_entry_if(&clash, None).unwrap_err().code(),
        "conflict",
        "a create must not silently replace an entry that is already there"
    );

    let v1 = store.get_entry(mine.id).unwrap().updated_at;

    // The other writer saves first, moving the stored version on.
    let mut theirs = mine.clone();
    theirs.body = RichDoc::from_plain_text("v2 from the other window");
    theirs.updated_at = jiff::Timestamp::now();
    store.put_entry_if(&theirs, Some(v1)).expect("a write from the version we read must land");
    let v2 = store.get_entry(mine.id).unwrap().updated_at;

    // Now ours, still believing the world is at v1.
    let mut stale = mine.clone();
    stale.body = RichDoc::from_plain_text("v2 from this window");
    stale.updated_at = jiff::Timestamp::now();
    assert_eq!(
        store.put_entry_if(&stale, Some(v1)).unwrap_err().code(),
        "conflict",
        "a write from a version that has been superseded must be refused"
    );
    assert_eq!(
        store.get_entry(mine.id).unwrap().body.plain_text(),
        "v2 from the other window",
        "a refused write must change nothing"
    );

    // Re-reading and retrying against the current version succeeds, which is
    // what makes the conflict recoverable rather than a dead end.
    stale.updated_at = jiff::Timestamp::now();
    store.put_entry_if(&stale, Some(v2)).expect("a retry from the current version must land");

    // An update whose row has been deleted underneath it is also a conflict:
    // recreating it silently would undo somebody's deletion.
    let current = store.get_entry(mine.id).unwrap().updated_at;
    store.delete_entry(mine.id).unwrap();
    assert_eq!(
        store.put_entry_if(&stale, Some(current)).unwrap_err().code(),
        "conflict",
        "an update must not resurrect a deleted entry"
    );

    // And the unconditional write is still available for the deliberate
    // "keep mine", which is what the interface offers on a conflict.
    store.put_entry(&stale).expect("an unconditional write must always be allowed");

    cleanup(store);
}

fn entry_get_missing_is_not_found(store: &dyn JournalStore) {
    let err = store.get_entry(EntryId::new()).unwrap_err();
    assert_eq!(err.code(), "not_found", "missing entry must report not_found, got {err}");
}

fn list_entries_filters_and_sorts(store: &dyn JournalStore) {
    let a = seeded_journal(store, "A");
    let b = seeded_journal(store, "B");

    let e1 = seeded_entry(store, a.id, date(2024, 1, 10), "oldest");
    let e2 = seeded_entry(store, a.id, date(2024, 6, 1), "middle");
    let e3 = seeded_entry(store, b.id, date(2024, 12, 25), "newest");

    let mut starred = store.get_entry(e2.id).unwrap();
    starred.starred = true;
    starred.tags = vec!["Highlight".into()];
    store.put_entry(&starred).unwrap();

    let ids = |q: &EntryQuery| -> Vec<EntryId> {
        store.list_entries(q).unwrap().into_iter().map(|r| r.id).collect()
    };

    assert_eq!(ids(&EntryQuery::default()), [e3.id, e2.id, e1.id], "default sort is date desc");

    assert_eq!(
        ids(&EntryQuery { sort: SortOrder::DateAsc, ..Default::default() }),
        [e1.id, e2.id, e3.id]
    );

    assert_eq!(ids(&EntryQuery::in_journal(a.id)), [e2.id, e1.id], "journal filter");

    assert_eq!(
        ids(&EntryQuery {
            from: Some(date(2024, 6, 1)),
            to: Some(date(2024, 6, 30)),
            ..Default::default()
        }),
        [e2.id],
        "date range is inclusive on both ends"
    );

    assert_eq!(
        ids(&EntryQuery { starred: Some(true), ..Default::default() }),
        [e2.id],
        "starred filter"
    );

    assert_eq!(
        ids(&EntryQuery { tags: vec!["highlight".into()], ..Default::default() }),
        [e2.id],
        "tag filter must be case-insensitive"
    );

    assert!(ids(&EntryQuery { tags: vec!["nonexistent".into()], ..Default::default() }).is_empty());

    cleanup(store);
}

fn list_entries_paginates(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Long");
    let mut ids = Vec::new();
    for d in 1..=10u8 {
        ids.push(seeded_entry(store, j.id, date(2024, 1, d as i8), &format!("day {d}")).id);
    }

    let page = |offset, limit| -> Vec<EntryId> {
        store
            .list_entries(&EntryQuery {
                sort: SortOrder::DateAsc,
                offset,
                limit: Some(limit),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    };

    assert_eq!(page(0, 3), ids[0..3]);
    assert_eq!(page(3, 3), ids[3..6]);
    assert_eq!(page(9, 3), ids[9..10], "a partial final page is fine");
    assert!(page(50, 3).is_empty(), "an offset past the end must not panic");

    cleanup(store);
}

fn blobs_are_content_addressed_and_deduplicated(store: &dyn JournalStore) {
    let bytes = b"\x89PNG\r\n\x1a\n and then some pixels";
    let id = store.put_blob(bytes).unwrap();

    assert_eq!(id, BlobId::of(bytes), "blob id must be the BLAKE3 of the plaintext");
    assert!(store.has_blob(id).unwrap());
    assert_eq!(store.get_blob(id).unwrap(), bytes, "blob must round-trip byte-identically");

    let again = store.put_blob(bytes).unwrap();
    assert_eq!(again, id, "identical bytes must deduplicate");
    assert_eq!(store.list_blobs().unwrap().len(), 1);

    let other = store.put_blob(b"different").unwrap();
    assert_ne!(other, id);
    assert_eq!(store.list_blobs().unwrap().len(), 2);

    store.delete_blob(id).unwrap();
    assert!(!store.has_blob(id).unwrap());
    assert!(store.get_blob(id).is_err());

    cleanup(store);
}

fn blob_ranges_match_the_full_payload(store: &dyn JournalStore) {
    // 900 KiB spans several chunks in the chunk-encrypted backends, so this
    // exercises boundary-straddling reads rather than a single-chunk case.
    let mut data = Vec::with_capacity(900 * 1024);
    let mut x: u32 = 0xdead_beef;
    for _ in 0..900 * 1024 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        data.push((x >> 24) as u8);
    }
    let id = store.put_blob(&data).unwrap();
    assert_eq!(
        store.blob_len(id).unwrap(),
        data.len() as u64,
        "blob_len must be the plaintext length"
    );

    for (off, len) in [(0u64, 16u64), (1, 1), (262_143, 4), (262_144, 100), (700_000, 200_000)] {
        assert_eq!(
            store.get_blob_range(id, off, len).unwrap(),
            data[off as usize..(off + len) as usize],
            "range ({off}, {len}) must match the full payload"
        );
    }
    // Clamping, not erroring, past the end.
    assert_eq!(
        store.get_blob_range(id, data.len() as u64 - 5, 500).unwrap(),
        data[data.len() - 5..]
    );
    assert!(store.get_blob_range(id, data.len() as u64, 10).unwrap().is_empty());

    store.delete_blob(id).unwrap();
    cleanup(store);
}

fn blob_get_missing_is_not_found(store: &dyn JournalStore) {
    let missing = BlobId::of(b"never stored");
    assert!(!store.has_blob(missing).unwrap());
    let err = store.get_blob(missing).unwrap_err();
    assert_eq!(err.code(), "not_found", "missing blob must report not_found, got {err}");
}

fn large_blob_round_trips(store: &dyn JournalStore) {
    // 4 MiB — a plausible phone photo, and large enough to catch backends
    // that quietly truncate or that mishandle chunked encryption.
    let mut bytes = Vec::with_capacity(4 << 20);
    let mut x: u32 = 0x9e37_79b9;
    for _ in 0..(4 << 20) {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push((x >> 24) as u8);
    }
    let id = store.put_blob(&bytes).unwrap();
    assert_eq!(store.get_blob(id).unwrap(), bytes, "a 4 MiB blob must round-trip intact");
    store.delete_blob(id).unwrap();

    cleanup(store);
}

fn deleting_a_journal_removes_its_entries(store: &dyn JournalStore) {
    let keep = seeded_journal(store, "Keep");
    let drop = seeded_journal(store, "Drop");
    let survivor = seeded_entry(store, keep.id, date(2024, 2, 2), "still here");
    let doomed = seeded_entry(store, drop.id, date(2024, 2, 3), "not for long");

    store.delete_journal(drop.id).unwrap();

    assert!(store.get_entry(doomed.id).is_err(), "entries must cascade with their journal");
    assert!(store.get_entry(survivor.id).is_ok(), "other journals must be untouched");
    assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 1);

    cleanup(store);
}

fn garbage_collection_keeps_referenced_blobs(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Photos");
    let used = store.put_blob(b"in use").unwrap();
    let orphan = store.put_blob(b"nobody wants me").unwrap();

    let mut e = Entry::new(j.id, "UTC");
    e.body = RichDoc(json!({
        "type": "doc",
        "content": [{"type": MEDIA_NODE,
                     "attrs": {"blob": used.to_hex(), "kind": "image"}}]
    }));
    store.put_entry(&e).unwrap();

    // A grace period protects blobs younger than it, referenced or not.
    // Both of these were written a moment ago, so an hour's grace must
    // collect neither -- this is what keeps GC from eating the image that
    // has been pasted into a draft but not yet saved.
    assert_eq!(
        store.collect_garbage(std::time::Duration::from_secs(3600)).unwrap(),
        0,
        "nothing younger than the grace period may be collected"
    );
    assert!(store.has_blob(orphan).unwrap(), "a young orphan must survive a graced GC");

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert_eq!(removed, 1, "exactly the unreferenced blob should be collected");
    assert!(store.has_blob(used).unwrap(), "a referenced blob must survive GC");
    assert!(!store.has_blob(orphan).unwrap(), "an unreferenced blob must be collected");

    // GC must be safe to run repeatedly.
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 0);

    cleanup(store);
}

fn stats_reflect_contents(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Counting");
    seeded_entry(store, j.id, date(2024, 3, 1), "one");
    seeded_entry(store, j.id, date(2024, 3, 2), "two");
    store.put_blob(b"a blob").unwrap();

    let s = store.stats().unwrap();
    assert_eq!(s.journals, 1);
    assert_eq!(s.entries, 2);
    assert_eq!(s.blobs, 1);
    assert!(s.blob_bytes > 0, "blob_bytes should report on-disk size");

    cleanup(store);
}

fn unicode_survives_a_round_trip(store: &dyn JournalStore) {
    let j = seeded_journal(store, "\u{65e5}\u{8a18}");
    // Emoji with ZWJ + skin tone, RTL text, combining marks, and a NUL-free
    // but control-character-adjacent string.
    let tricky = "\u{1f469}\u{200d}\u{1f4bb} \u{5bb6}\u{65cf} \u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629} e\u{301}cole \u{1f1ef}\u{1f1f5}";
    let mut e = Entry::new(j.id, "Asia/Tokyo");
    e.title = tricky.into();
    e.body = RichDoc::from_plain_text(tricky);
    e.tags = vec!["\u{30bf}\u{30b0}".into()];
    store.put_entry(&e).unwrap();

    let got = store.get_entry(e.id).unwrap();
    assert_eq!(got.title, tricky, "unicode title must survive");
    assert_eq!(got.body.plain_text(), tricky, "unicode body must survive");
    assert_eq!(got.tags, e.tags, "unicode tags must survive");
    assert_eq!(store.get_journal(j.id).unwrap().name, "\u{65e5}\u{8a18}");

    cleanup(store);
}

// ── The task domain ──────────────────────────────────────────────────────

fn task_cleanup(store: &dyn TaskStore) {
    for p in store.list_projects().expect("list_projects") {
        store.delete_project(p.id).expect("delete_project");
    }
    for t in store.list_tasks(&TaskQuery::default()).expect("list_tasks") {
        // Deleting a parent takes its children, so a child may already be
        // gone by the time this reaches it. That is not a failure.
        let _ = store.delete_task(t.id);
    }
    for b in store.list_blocks(&BlockQuery::default()).expect("list_blocks") {
        store.delete_block(b.id).expect("delete_block");
    }
    assert!(store.list_projects().unwrap().is_empty(), "cleanup left projects behind");
    assert!(
        store.list_tasks(&TaskQuery::default()).unwrap().is_empty(),
        "cleanup left tasks behind"
    );
    assert!(
        store.list_blocks(&BlockQuery::default()).unwrap().is_empty(),
        "cleanup left time blocks behind"
    );
}

fn seeded_project(store: &dyn TaskStore, name: &str) -> Project {
    let p = Project::new(name);
    store.put_project(&p).expect("put_project");
    p
}

fn seeded_task(store: &dyn TaskStore, project: Option<ProjectId>, title: &str) -> Task {
    let t = Task::new(title).in_project(project);
    store.put_task(&t).expect("put_task");
    t
}

fn tasks_start_empty(store: &dyn TaskStore) {
    assert!(store.list_projects().unwrap().is_empty(), "a fresh store must have no projects");
    assert!(
        store.list_tasks(&TaskQuery::default()).unwrap().is_empty(),
        "a fresh store must have no tasks"
    );
    assert!(
        store.list_blocks(&BlockQuery::default()).unwrap().is_empty(),
        "a fresh store must have no time blocks"
    );
    assert_eq!(store.task_stats(date(2026, 6, 15)).unwrap().tasks, 0);
}

fn project_crud(store: &dyn TaskStore) {
    let mut p = Project::new("Kitchen").with_color("#0f766e").with_icon("\u{1f6e0}");
    p.notes = "The one that never ends".into();
    p.priority = Priority::High;
    p.start_date = Some(date(2026, 1, 5));
    p.due_date = Some(date(2026, 4, 1));
    p.estimate_minutes = Some(4_800);
    p.tags = vec!["home".into(), "money".into()];
    p.sort_order = 2;
    store.put_project(&p).unwrap();

    assert_eq!(store.get_project(p.id).unwrap(), p, "project must round-trip unchanged");

    p.set_status(ProjectStatus::Done);
    store.put_project(&p).unwrap();
    let got = store.get_project(p.id).unwrap();
    assert_eq!(got.status, ProjectStatus::Done, "put must upsert");
    assert!(got.completed_at.is_some());
    assert_eq!(store.list_projects().unwrap().len(), 1, "upsert must not duplicate");

    // Archived projects are still stored; hiding them is the caller's job.
    p.set_status(ProjectStatus::Archived);
    store.put_project(&p).unwrap();
    assert_eq!(store.list_projects().unwrap().len(), 1, "archiving must not delete");

    store.delete_project(p.id).unwrap();
    assert!(store.get_project(p.id).is_err(), "project must be gone after delete");
    task_cleanup(store);
}

fn task_round_trips_every_field(store: &dyn TaskStore) {
    let p = seeded_project(store, "Move house");
    let parent = seeded_task(store, Some(p.id), "Pack the study");

    let mut t = Task::new("Box up the books").in_project(p.id).under(parent.id);
    t.notes = "Heavy. Use the small boxes.\n\nTwo lines, deliberately.".into();
    t.status = TaskStatus::Doing;
    t.priority = Priority::Urgent;
    t.start_date = Some(date(2026, 5, 1));
    t.due_date = Some(date(2026, 5, 9));
    t.due_time = Some(time(17, 30, 0, 0));
    t.estimate_minutes = Some(120);
    t.tags = vec!["moving".into(), "\u{1f4da}".into()];
    t.sort_order = 7;
    store.put_task(&t).unwrap();

    assert_eq!(store.get_task(t.id).unwrap(), t, "task must round-trip every field unchanged");
    task_cleanup(store);
}

fn task_put_is_idempotent(store: &dyn TaskStore) {
    let mut t = Task::new("Water the plants");
    store.put_task(&t).unwrap();
    store.put_task(&t).unwrap();
    assert_eq!(store.list_tasks(&TaskQuery::default()).unwrap().len(), 1, "put must not duplicate");

    t.title = "Water the plants properly".into();
    store.put_task(&t).unwrap();
    assert_eq!(store.get_task(t.id).unwrap().title, "Water the plants properly");
    assert_eq!(store.list_tasks(&TaskQuery::default()).unwrap().len(), 1);
    task_cleanup(store);
}

fn missing_task_records_are_not_found(store: &dyn TaskStore) {
    for code in [
        store.get_project(ProjectId::new()).unwrap_err().code(),
        store.get_task(TaskId::new()).unwrap_err().code(),
        store.get_block(BlockId::new()).unwrap_err().code(),
    ] {
        assert_eq!(code, "not_found", "a missing task record must report not_found");
    }
    // Deleting something that is not there is a no-op, not an error: two
    // clients racing to tick off the same task should both succeed.
    store.delete_task(TaskId::new()).expect("deleting a missing task must be a no-op");
    store.delete_block(BlockId::new()).expect("deleting a missing block must be a no-op");
    store.delete_project(ProjectId::new()).expect("deleting a missing project must be a no-op");
}

fn list_tasks_filters_and_sorts(store: &dyn TaskStore) {
    let p = seeded_project(store, "Filtering");
    let other = seeded_project(store, "Elsewhere");

    let mut a = Task::new("alpha").in_project(p.id);
    a.due_date = Some(date(2026, 2, 3));
    a.priority = Priority::High;
    a.tags = vec!["Work".into()];
    a.sort_order = 1;

    let mut b = Task::new("bravo").in_project(p.id);
    b.status = TaskStatus::Doing;
    b.due_date = Some(date(2026, 2, 1));
    b.sort_order = 0;

    let mut c = Task::new("charlie").in_project(other.id);
    c.set_status(TaskStatus::Done);

    let d = Task::new("delta"); // inbox, no project, no due date

    for t in [&a, &b, &c, &d] {
        store.put_task(t).unwrap();
    }

    let titles = |q: &TaskQuery| -> Vec<String> {
        store.list_tasks(q).unwrap().into_iter().map(|t| t.title).collect()
    };

    assert_eq!(
        titles(&TaskQuery::in_project(p.id)),
        ["bravo", "alpha"],
        "a project query must be scoped and in manual order"
    );
    assert_eq!(
        titles(&TaskQuery { project: ProjectScope::Inbox, ..Default::default() }),
        ["delta"],
        "the inbox is tasks with no project"
    );
    assert_eq!(
        titles(&TaskQuery { statuses: vec![TaskStatus::Done], ..Default::default() }),
        ["charlie"],
    );
    assert_eq!(titles(&TaskQuery::open()).len(), 3, "open excludes only done and cancelled");
    assert_eq!(
        titles(&TaskQuery { tags: vec!["work".into()], ..Default::default() }),
        ["alpha"],
        "tag filtering must ignore case"
    );
    assert_eq!(
        titles(&TaskQuery { text: "BRAV".into(), ..Default::default() }),
        ["bravo"],
        "text filtering must ignore case"
    );
    assert_eq!(
        titles(&TaskQuery { priority_at_least: Some(Priority::High), ..Default::default() }),
        ["alpha"],
    );
    assert_eq!(
        titles(&TaskQuery { has_due: Some(false), ..Default::default() }),
        ["charlie", "delta"],
        "has_due=false must find the undated ones"
    );
    assert_eq!(
        titles(&TaskQuery {
            due_to: Some(date(2026, 2, 2)),
            sort: TaskSort::DueAsc,
            ..Default::default()
        }),
        ["bravo"],
        "an undated task must not fall inside a date window"
    );
    assert_eq!(
        titles(&TaskQuery { sort: TaskSort::TitleAsc, ..Default::default() }),
        ["alpha", "bravo", "charlie", "delta"],
    );
    assert_eq!(
        titles(&TaskQuery { sort: TaskSort::PriorityDesc, ..Default::default() })[0],
        "alpha",
        "the most important task must come first",
    );
    // Undated tasks sort last under DueAsc rather than first.
    let by_due = titles(&TaskQuery { sort: TaskSort::DueAsc, ..Default::default() });
    assert_eq!(&by_due[..2], ["bravo", "alpha"]);

    task_cleanup(store);
}

fn list_tasks_paginates(store: &dyn TaskStore) {
    let p = seeded_project(store, "Long");
    for i in 0..20i32 {
        let mut t = Task::new(format!("task {i:02}")).in_project(p.id);
        t.sort_order = i;
        store.put_task(&t).unwrap();
    }
    let window = TaskQuery {
        offset: 5,
        limit: Some(4),
        sort: TaskSort::Manual,
        ..TaskQuery::in_project(p.id)
    };
    let titles: Vec<String> =
        store.list_tasks(&window).unwrap().into_iter().map(|t| t.title).collect();
    assert_eq!(titles, ["task 05", "task 06", "task 07", "task 08"]);

    let past_the_end = TaskQuery { offset: 999, ..TaskQuery::in_project(p.id) };
    assert!(
        store.list_tasks(&past_the_end).unwrap().is_empty(),
        "paging past the end yields nothing rather than failing"
    );

    let unlimited = TaskQuery { offset: 18, ..TaskQuery::in_project(p.id) };
    assert_eq!(store.list_tasks(&unlimited).unwrap().len(), 2, "offset without limit must work");

    task_cleanup(store);
}

fn batch_writes_land_together(store: &dyn TaskStore) {
    // What a board reorder looks like: renumber a column in one call.
    let p = seeded_project(store, "Reordering");
    let mut rows: Vec<Task> = (0..5)
        .map(|i| {
            let mut t = Task::new(format!("card {i}")).in_project(p.id);
            t.sort_order = i;
            t
        })
        .collect();
    store.put_tasks(&rows).unwrap();
    assert_eq!(store.list_tasks(&TaskQuery::in_project(p.id)).unwrap().len(), 5);

    rows.reverse();
    for (i, t) in rows.iter_mut().enumerate() {
        t.sort_order = i as i32;
    }
    store.put_tasks(&rows).unwrap();

    let titles: Vec<String> = store
        .list_tasks(&TaskQuery::in_project(p.id))
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(titles, ["card 4", "card 3", "card 2", "card 1", "card 0"]);

    // An empty batch is a legitimate no-op, not an error.
    store.put_tasks(&[]).unwrap();
    task_cleanup(store);
}

fn subtasks_are_reachable_from_their_parent(store: &dyn TaskStore) {
    let p = seeded_project(store, "Nesting");
    let parent = seeded_task(store, Some(p.id), "parent");
    let mut child = Task::new("child").in_project(p.id).under(parent.id);
    child.sort_order = 0;
    let mut grandchild = Task::new("grandchild").in_project(p.id).under(child.id);
    grandchild.sort_order = 0;
    store.put_tasks(&[child.clone(), grandchild.clone()]).unwrap();

    let kids: Vec<String> = store
        .list_tasks(&TaskQuery::children_of(parent.id))
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(kids, ["child"], "children_of must return one level, not the whole subtree");

    let top: Vec<String> = store
        .list_tasks(&TaskQuery { parent: ParentScope::TopLevel, ..TaskQuery::in_project(p.id) })
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(top, ["parent"], "top level must exclude subtasks at any depth");

    assert_eq!(
        store.list_tasks(&TaskQuery::in_project(p.id)).unwrap().len(),
        3,
        "an unscoped project query returns every level"
    );
    task_cleanup(store);
}

fn deleting_a_task_takes_its_subtrees_with_it(store: &dyn TaskStore) {
    let p = seeded_project(store, "Cascades");
    let parent = seeded_task(store, Some(p.id), "parent");
    let child = Task::new("child").in_project(p.id).under(parent.id);
    let grandchild = Task::new("grandchild").in_project(p.id).under(child.id);
    let bystander = seeded_task(store, Some(p.id), "bystander");
    store.put_tasks(&[child.clone(), grandchild.clone()]).unwrap();

    store.delete_task(parent.id).unwrap();

    for gone in [parent.id, child.id, grandchild.id] {
        assert!(store.get_task(gone).is_err(), "the whole subtree must go, not just the root");
    }
    assert!(store.get_task(bystander.id).is_ok(), "a sibling must be untouched");
    task_cleanup(store);
}

fn deleting_a_project_takes_its_tasks_with_it(store: &dyn TaskStore) {
    let doomed = seeded_project(store, "Doomed");
    let kept = seeded_project(store, "Kept");
    let inside = seeded_task(store, Some(doomed.id), "inside");
    let nested = Task::new("nested").in_project(doomed.id).under(inside.id);
    store.put_task(&nested).unwrap();
    let elsewhere = seeded_task(store, Some(kept.id), "elsewhere");
    let loose = seeded_task(store, None, "loose");

    store.delete_project(doomed.id).unwrap();

    assert!(store.get_project(doomed.id).is_err());
    assert!(store.get_task(inside.id).is_err(), "a project takes its tasks with it");
    assert!(store.get_task(nested.id).is_err(), "and their subtasks");
    assert!(store.get_task(elsewhere.id).is_ok(), "another project must be untouched");
    assert!(store.get_task(loose.id).is_ok(), "the inbox must be untouched");
    task_cleanup(store);
}

fn time_blocks_round_trip_and_query_by_day(store: &dyn TaskStore) {
    let p = seeded_project(store, "Timed");
    let t = seeded_task(store, Some(p.id), "the work");

    // 09:00 and 14:00 UTC on one day, and one the day after.
    let base = "2026-06-15T09:00:00Z".parse::<Timestamp>().unwrap();
    let hour = jiff::SignedDuration::from_hours(1);

    let mut morning = TimeBlock::new(BlockSubject::Task { id: t.id }, base, 90, "UTC");
    morning.notes = "the good bit of the day".into();
    morning.tags = vec!["deep".into()];
    let afternoon = TimeBlock::new(BlockSubject::Task { id: t.id }, base + hour * 5, 60, "UTC")
        .of_kind(BlockKind::Actual);
    let tomorrow = TimeBlock::new(BlockSubject::Project { id: p.id }, base + hour * 24, 30, "UTC");
    let mut errand = TimeBlock::new(BlockSubject::Adhoc, base + hour * 3, 45, "UTC");
    errand.title = "dentist".into();

    for b in [&morning, &afternoon, &tomorrow, &errand] {
        store.put_block(b).unwrap();
    }

    assert_eq!(store.get_block(morning.id).unwrap(), morning, "a block must round-trip unchanged");

    let day = date(2026, 6, 15);
    let on_the_day = store.list_blocks(&BlockQuery::between(day, day)).unwrap();
    assert_eq!(on_the_day.len(), 3, "the day query must not reach into tomorrow");
    assert!(
        on_the_day.windows(2).all(|w| w[0].start <= w[1].start),
        "blocks must come back in time order"
    );

    let for_task = store.list_blocks(&BlockQuery::for_task(t.id)).unwrap();
    assert_eq!(for_task.len(), 2, "a task's own blocks, whatever day they are on");

    let logged = store
        .list_blocks(&BlockQuery { kind: Some(BlockKind::Actual), ..Default::default() })
        .unwrap();
    assert_eq!(logged.len(), 1, "planned and actual must be distinguishable");
    assert_eq!(logged[0].minutes(), 60);

    let for_project =
        store.list_blocks(&BlockQuery { project_id: Some(p.id), ..Default::default() }).unwrap();
    assert_eq!(for_project.len(), 1, "a project block is the one addressed to the project");

    // Upsert, not insert.
    let mut moved = morning.clone();
    moved.start = base + hour;
    moved.end = moved.start + hour;
    store.put_block(&moved).unwrap();
    assert_eq!(store.list_blocks(&BlockQuery::default()).unwrap().len(), 4);
    assert_eq!(store.get_block(morning.id).unwrap().minutes(), 60);

    store.delete_block(errand.id).unwrap();
    assert!(store.get_block(errand.id).is_err());

    task_cleanup(store);
}

fn deleting_a_task_removes_its_time_blocks(store: &dyn TaskStore) {
    // Orphaned blocks would quietly corrupt every time-spent total
    // afterwards, which is the one question this domain exists to answer.
    let p = seeded_project(store, "Accounting");
    let doomed = seeded_task(store, Some(p.id), "doomed");
    let child = Task::new("child").in_project(p.id).under(doomed.id);
    store.put_task(&child).unwrap();
    let kept = seeded_task(store, Some(p.id), "kept");

    let base = "2026-06-15T09:00:00Z".parse::<Timestamp>().unwrap();
    for subject in [
        BlockSubject::Task { id: doomed.id },
        BlockSubject::Task { id: child.id },
        BlockSubject::Task { id: kept.id },
    ] {
        store.put_block(&TimeBlock::new(subject, base, 30, "UTC")).unwrap();
    }
    assert_eq!(store.list_blocks(&BlockQuery::default()).unwrap().len(), 3);

    store.delete_task(doomed.id).unwrap();
    let left = store.list_blocks(&BlockQuery::default()).unwrap();
    assert_eq!(left.len(), 1, "the deleted task and its child took their blocks with them");
    assert_eq!(left[0].subject.task_id(), Some(kept.id));

    // And a project delete does the same, transitively.
    store.delete_project(p.id).unwrap();
    assert!(
        store.list_blocks(&BlockQuery::default()).unwrap().is_empty(),
        "a deleted project must leave no time behind either"
    );
    task_cleanup(store);
}

fn task_stats_reflect_contents(store: &dyn TaskStore) {
    let p = seeded_project(store, "Counting");
    let loose = seeded_task(store, None, "in the inbox");
    let mut archived = Project::new("Old");
    archived.set_status(ProjectStatus::Archived);
    store.put_project(&archived).unwrap();

    let open = seeded_task(store, Some(p.id), "open");
    let mut done = Task::new("done").in_project(p.id);
    done.set_status(TaskStatus::Done);
    let mut cancelled = Task::new("cancelled").in_project(p.id);
    cancelled.set_status(TaskStatus::Cancelled);
    // Dated either side of the day the stats are taken on, plus one with no
    // deadline at all -- which must fall outside both counts.
    let mut late = Task::new("late").in_project(p.id);
    late.due_date = Some(date(2026, 6, 14));
    let mut today_task = Task::new("today").in_project(p.id);
    today_task.due_date = Some(date(2026, 6, 15));
    let mut soon = Task::new("soon").in_project(p.id);
    soon.due_date = Some(date(2026, 6, 16));
    let dated = [late.id, today_task.id, soon.id];
    store.put_tasks(&[done, cancelled, late, today_task, soon]).unwrap();

    let base = "2026-06-15T09:00:00Z".parse::<Timestamp>().unwrap();
    store.put_block(&TimeBlock::new(BlockSubject::Task { id: open.id }, base, 90, "UTC")).unwrap();
    store
        .put_block(
            &TimeBlock::new(BlockSubject::Task { id: open.id }, base, 45, "UTC")
                .of_kind(BlockKind::Actual),
        )
        .unwrap();

    let s = store.task_stats(date(2026, 6, 15)).unwrap();
    assert_eq!(s.projects, 2);
    assert_eq!(s.active_projects, 1, "an archived project is not an active one");
    assert_eq!(s.tasks, 7);
    assert_eq!(s.open_tasks, 5, "cancelled counts as closed, but not as done");
    assert_eq!(s.due_today, 2, "due today counts today's and everything overdue");
    assert_eq!(s.overdue, 1, "overdue is the deadlines already missed");
    assert_eq!(
        store.task_stats(date(2026, 6, 20)).unwrap().overdue,
        3,
        "the counts move with the day they are asked about",
    );
    assert_eq!(s.done_tasks, 1);
    assert_eq!(s.blocks, 2);
    assert_eq!(s.planned_minutes, 90);
    assert_eq!(s.logged_minutes, 45);

    // The per-project roll-up: outstanding work only, with the inbox under
    // the `None` key, and no row at all for a project with nothing left.
    let mut by_project = s.open_by_project.clone();
    by_project.sort_by_key(|c| c.project_id.map(|id| id.to_string()));
    assert_eq!(
        by_project,
        vec![
            ProjectTaskCount { project_id: None, open: 1 },
            ProjectTaskCount { project_id: Some(p.id), open: 4 },
        ],
        "open_by_project must count only outstanding work, inbox included",
    );

    // Finish the last open task in the project and its row disappears
    // entirely rather than reporting zero.
    for id in std::iter::once(open.id).chain(dated) {
        let mut t = store.get_task(id).unwrap();
        t.set_status(TaskStatus::Done);
        store.put_task(&t).unwrap();
    }
    let s = store.task_stats(date(2026, 6, 15)).unwrap();
    assert_eq!(
        s.open_by_project.iter().filter(|c| c.project_id == Some(p.id)).count(),
        0,
        "a project with nothing open must not be listed",
    );
    assert!(s.open_by_project.iter().any(|c| c.project_id.is_none() && c.open == 1));
    assert_eq!((s.due_today, s.overdue), (0, 0), "finished work is never due");
    let _ = loose;

    task_cleanup(store);
}

fn unicode_survives_a_task_round_trip(store: &dyn TaskStore) {
    let tricky = "\u{1f469}\u{200d}\u{1f4bb} \u{5bb6}\u{65cf} \u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629} e\u{301}cole \u{1f1ef}\u{1f1f5}";
    let mut p = Project::new(tricky);
    p.notes = tricky.into();
    p.tags = vec!["\u{30bf}\u{30b0}".into()];
    store.put_project(&p).unwrap();

    let mut t = Task::new(tricky).in_project(p.id);
    t.notes = tricky.into();
    t.tags = vec!["\u{30bf}\u{30b0}".into()];
    store.put_task(&t).unwrap();

    assert_eq!(store.get_project(p.id).unwrap().name, tricky, "unicode project name must survive");
    let got = store.get_task(t.id).unwrap();
    assert_eq!(got.title, tricky, "unicode task title must survive");
    assert_eq!(got.notes, tricky, "unicode notes must survive");
    assert_eq!(got.tags, t.tags, "unicode tags must survive");

    // And the filters must find it, which is the part a naive byte-wise
    // lowercase would break.
    let found = store
        .list_tasks(&TaskQuery { text: "\u{5bb6}\u{65cf}".into(), ..Default::default() })
        .unwrap();
    assert_eq!(found.len(), 1, "text search must work on non-ASCII");

    task_cleanup(store);
}

// ── The calendar domain ──────────────────────────────────────────────────

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
fn sample_event(cal: CalendarId, uid: &str, from: Date, to: Date) -> Event {
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
    let err = store.get_calendar(CalendarId::new()).unwrap_err();
    assert!(matches!(err, crate::Error::NotFound { .. }), "expected NotFound, got {err:?}");
    let err = store.get_event(EventId::new()).unwrap_err();
    assert!(matches!(err, crate::Error::NotFound { .. }), "expected NotFound, got {err:?}");
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

// ---------------------------------------------------------------------------
// The purpose domain
// ---------------------------------------------------------------------------

/// The purpose half of the suite. Called by [`run_all`] when the backend has
/// a [`PurposeStore`](super::purpose::PurposeStore).
///
/// Handed the whole journal store, because half of what it checks is that
/// *other* domains report their purpose correctly. The store must be empty
/// of roles on entry; it is left empty on success.
pub fn run_purpose_suite(store: &dyn JournalStore) {
    eprintln!("--- purpose conformance suite ---");

    purpose_starts_empty(store);
    role_crud(store);
    missing_purpose_records_are_not_found(store);
    goal_round_trips_and_filters(store);
    batch_goal_writes_land_together(store);
    a_role_with_goals_under_it_refuses_to_be_deleted(store);
    role_goal_counts_are_answered_by_the_backend(store);
    a_purpose_survives_a_round_trip_on_every_record(store);
    time_is_attributed_down_the_inheritance_chain(store);
    unattributed_time_is_reported_rather_than_dropped(store);
    clearing_a_purpose_removes_it_from_the_reports(store);
    deleting_a_record_takes_its_purpose_with_it(store);
    goal_activity_counts_what_points_at_it(store);
    events_are_attributed_by_their_calendar(store);
    unicode_survives_a_purpose_round_trip(store);

    purpose_cleanup(store);
    eprintln!("--- purpose suite passed ---");
}

fn purpose_store(store: &dyn JournalStore) -> &dyn super::purpose::PurposeStore {
    store.purpose().expect("the purpose suite needs a purpose store")
}

fn purpose_cleanup(store: &dyn JournalStore) {
    let p = purpose_store(store);
    for goal in p.list_goals(&GoalQuery::default()).expect("list_goals") {
        p.delete_goal(goal.id).expect("delete_goal");
    }
    for role in p.list_roles().expect("list_roles") {
        p.delete_role(role.id).expect("delete_role");
    }
    assert!(p.list_roles().unwrap().is_empty(), "cleanup left roles behind");
    assert!(p.list_goals(&GoalQuery::default()).unwrap().is_empty(), "cleanup left goals behind");
}

fn seeded_role(store: &dyn JournalStore, name: &str) -> LifeRole {
    let role = LifeRole::new(name);
    purpose_store(store).put_role(&role).expect("put_role");
    role
}

fn seeded_goal(store: &dyn JournalStore, role: &LifeRole, title: &str) -> Goal {
    let goal = Goal::new(role.id, title);
    purpose_store(store).put_goal(&goal).expect("put_goal");
    goal
}

fn purpose_starts_empty(store: &dyn JournalStore) {
    let p = purpose_store(store);
    assert!(p.list_roles().unwrap().is_empty(), "a fresh store must have no roles");
    assert!(p.list_goals(&GoalQuery::default()).unwrap().is_empty(), "a fresh store has no goals");
}

fn role_crud(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let mut role = LifeRole::new("Parent").with_color("#0f766e").with_icon("\u{1f3e1}");
    role.notes = "the one that matters".into();
    role.sort_order = 3;
    p.put_role(&role).unwrap();

    assert_eq!(p.get_role(role.id).unwrap(), role, "a role must survive the round trip whole");
    assert_eq!(p.list_roles().unwrap().len(), 1);

    // Idempotent, as every `put_` in this codebase is.
    p.put_role(&role).unwrap();
    assert_eq!(p.list_roles().unwrap().len(), 1, "putting twice must not make two roles");

    role.name = "Father".into();
    role.archived = true;
    p.put_role(&role).unwrap();
    let back = p.get_role(role.id).unwrap();
    assert_eq!(back.name, "Father");
    assert!(back.archived, "an archived role is still listed; it is only hidden from pickers");
    assert_eq!(p.list_roles().unwrap().len(), 1, "archiving is not deleting");

    p.delete_role(role.id).unwrap();
    assert!(p.list_roles().unwrap().is_empty());
}

fn missing_purpose_records_are_not_found(store: &dyn JournalStore) {
    let p = purpose_store(store);
    assert!(
        matches!(p.get_role(RoleId::new()), Err(crate::Error::NotFound { .. })),
        "a role that was never written is not found, not an empty one"
    );
    assert!(matches!(p.get_goal(GoalId::new()), Err(crate::Error::NotFound { .. })));
    // Deleting what is not there is not an error: it is already true.
    p.delete_goal(GoalId::new()).expect("deleting a missing goal is a no-op");
    p.delete_role(RoleId::new()).expect("deleting a missing role is a no-op");
}

fn goal_round_trips_and_filters(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let parent = seeded_role(store, "Parent");
    let work = seeded_role(store, "Work");

    let mut riding = Goal::new(parent.id, "Viya rides without stabilisers");
    riding.notes = "start on the grass".into();
    riding.horizon = Some(date(2027, 3, 1));
    riding.sort_order = 1;
    p.put_goal(&riding).unwrap();

    let mut shipped = Goal::new(work.id, "ship the thing");
    shipped.set_status(GoalStatus::Done);
    p.put_goal(&shipped).unwrap();

    let mut later = Goal::new(parent.id, "teach her to swim");
    later.set_status(GoalStatus::Paused);
    p.put_goal(&later).unwrap();

    assert_eq!(p.get_goal(riding.id).unwrap(), riding, "every field must survive");

    let all = p.list_goals(&GoalQuery::default()).unwrap();
    assert_eq!(all.len(), 3);

    let under_parent = p.list_goals(&GoalQuery::under(parent.id)).unwrap();
    assert_eq!(under_parent.len(), 2);
    assert!(under_parent.iter().all(|g| g.role_id == parent.id));

    // Paused is open; done is not.
    let open = p.list_goals(&GoalQuery::open()).unwrap();
    assert_eq!(open.len(), 2, "a paused goal is still one you are pursuing");
    assert!(!open.iter().any(|g| g.id == shipped.id));

    // An undated goal is outside every horizon window rather than inside
    // all of them.
    let by_horizon = p
        .list_goals(&GoalQuery { horizon_to: Some(date(2027, 12, 31)), ..Default::default() })
        .unwrap();
    assert_eq!(by_horizon.len(), 1);
    assert_eq!(by_horizon[0].id, riding.id);

    let capped = p.list_goals(&GoalQuery { limit: Some(1), ..Default::default() }).unwrap();
    assert_eq!(capped.len(), 1);

    for g in [riding, shipped, later] {
        p.delete_goal(g.id).unwrap();
    }
    p.delete_role(parent.id).unwrap();
    p.delete_role(work.id).unwrap();
}

fn batch_goal_writes_land_together(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let role = seeded_role(store, "Batch");
    let goals: Vec<Goal> = (0..5).map(|i| Goal::new(role.id, format!("goal {i}"))).collect();
    p.put_goals(&goals).unwrap();
    assert_eq!(p.list_goals(&GoalQuery::under(role.id)).unwrap().len(), 5);

    // An empty batch is a no-op rather than an error, because "save the
    // selection" with nothing selected is a real call.
    p.put_goals(&[]).unwrap();

    for g in goals {
        p.delete_goal(g.id).unwrap();
    }
    p.delete_role(role.id).unwrap();
}

fn a_role_with_goals_under_it_refuses_to_be_deleted(store: &dyn JournalStore) {
    // The one parent in this vault that does not take its children with it.
    // A shelf is what its items are made of; a role is not what a year of
    // attributed hours is made of, and one click is the wrong distance from
    // losing them.
    let p = purpose_store(store);
    let role = seeded_role(store, "Held");
    let goal = seeded_goal(store, &role, "still wanted");

    let refused = p.delete_role(role.id);
    assert!(refused.is_err(), "a role with goals under it must not be deletable");
    assert!(p.get_role(role.id).is_ok(), "the refusal must leave the role alone");
    assert!(p.get_goal(goal.id).is_ok(), "and must certainly leave the goal alone");

    // ...and it becomes deletable once nothing points at it.
    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn role_goal_counts_are_answered_by_the_backend(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let role = seeded_role(store, "Counted");
    assert_eq!(p.count_goals(role.id).unwrap(), (0, 0));

    let a = seeded_goal(store, &role, "open one");
    let mut b = Goal::new(role.id, "finished one");
    b.set_status(GoalStatus::Done);
    p.put_goal(&b).unwrap();
    let mut c = Goal::new(role.id, "paused one");
    c.set_status(GoalStatus::Paused);
    p.put_goal(&c).unwrap();

    assert_eq!(
        p.count_goals(role.id).unwrap(),
        (3, 2),
        "three goals, two of them still being pursued"
    );

    for g in [a.id, b.id, c.id] {
        p.delete_goal(g).unwrap();
    }
    p.delete_role(role.id).unwrap();
}

fn a_purpose_survives_a_round_trip_on_every_record(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let role = seeded_role(store, "Round trip");
    let goal = seeded_goal(store, &role, "the goal");
    let by_goal = goal.purpose();
    let by_role = role.purpose();

    // A project and a task, pointing at a goal.
    if let Some(tasks) = store.tasks() {
        let mut project = Project::new("filed");
        project.purpose = Some(by_goal);
        tasks.put_project(&project).unwrap();
        assert_eq!(tasks.get_project(project.id).unwrap().purpose, Some(by_goal));

        let mut task = Task::new("filed too");
        task.purpose = Some(by_role);
        tasks.put_task(&task).unwrap();
        assert_eq!(
            tasks.get_task(task.id).unwrap().purpose,
            Some(by_role),
            "pointing straight at a role is not a degraded case of pointing at a goal"
        );

        let mut block = TimeBlock::new(
            BlockSubject::Adhoc,
            Timestamp::from_second(1_800_000_000).unwrap(),
            60,
            "UTC",
        );
        block.purpose = Some(by_goal);
        tasks.put_block(&block).unwrap();
        assert_eq!(tasks.get_block(block.id).unwrap().purpose, Some(by_goal));

        tasks.delete_block(block.id).unwrap();
        tasks.delete_task(task.id).unwrap();
        tasks.delete_project(project.id).unwrap();
    }

    // An entry.
    let journal = Journal::new("purpose");
    store.put_journal(&journal).unwrap();
    let mut entry = Entry::new(journal.id, "UTC");
    entry.purpose = Some(by_goal);
    store.put_entry(&entry).unwrap();
    assert_eq!(store.get_entry(entry.id).unwrap().purpose, Some(by_goal));
    store.delete_journal(journal.id).unwrap();

    // A shelf item.
    if let Some(library) = store.library() {
        let kind = seeded_kind(library, "purpose");
        let mut item = Item::new(kind.id, "the book for it");
        item.purpose = Some(by_goal);
        library.put_item(&item).unwrap();
        assert_eq!(library.get_item(item.id).unwrap().purpose, Some(by_goal));
        library.delete_kind(kind.id).unwrap();
    }

    // A subscribed calendar, which carries a role rather than a purpose.
    if let Some(calendars) = store.calendars() {
        let mut cal = Calendar::subscribed("work", "https://example.com/w.ics");
        cal.role_id = Some(role.id);
        calendars.put_calendar(&cal).unwrap();
        assert_eq!(calendars.get_calendar(cal.id).unwrap().role_id, Some(role.id));
        calendars.delete_calendar(cal.id).unwrap();
    }

    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn time_is_attributed_down_the_inheritance_chain(store: &dyn JournalStore) {
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Chain");
    let goal = seeded_goal(store, &role, "the goal");
    let other = seeded_goal(store, &role, "the other goal");

    // A project with a purpose, a task under it with none, and a block under
    // that with none: the block must be attributed to the project's goal.
    let mut project = Project::new("attributed");
    project.purpose = Some(goal.purpose());
    tasks.put_project(&project).unwrap();

    let mut inherits = Task::new("inherits");
    inherits.project_id = Some(project.id);
    tasks.put_task(&inherits).unwrap();

    // ...and a sibling task that overrides it, to prove the block does not
    // simply take the project's in every case.
    let mut overrides = Task::new("overrides");
    overrides.project_id = Some(project.id);
    overrides.purpose = Some(other.purpose());
    tasks.put_task(&overrides).unwrap();

    let day = date(2026, 9, 14);
    let at = day.at(9, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let inherited_block = TimeBlock::new(BlockSubject::Task { id: inherits.id }, at, 60, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&inherited_block).unwrap();

    let overridden_block = TimeBlock::new(BlockSubject::Task { id: overrides.id }, at, 30, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&overridden_block).unwrap();

    // A block with its own purpose, overriding both.
    let mut own = TimeBlock::new(BlockSubject::Task { id: inherits.id }, at, 15, "UTC")
        .of_kind(BlockKind::Planned);
    own.purpose = Some(role.purpose());
    tasks.put_block(&own).unwrap();

    let window = PurposeWindow::new(day, day);
    let rows = p.time_by_purpose(window).unwrap();

    let find = |want: Purpose| {
        rows.iter()
            .find(|r| r.purpose == Some(want))
            .unwrap_or_else(|| panic!("expected a row for {want:?}, got {rows:?}"))
    };

    assert_eq!(
        find(goal.purpose()).actual_minutes,
        60,
        "a block with no purpose and a task with none takes its project's"
    );
    assert_eq!(
        find(other.purpose()).actual_minutes,
        30,
        "a task's own purpose beats the project it is filed under"
    );
    let owned = find(role.purpose());
    assert_eq!(owned.planned_minutes, 15, "a block's own purpose beats everything above it");
    assert_eq!(owned.actual_minutes, 0, "planned and actual are counted apart, never summed");

    // Outside the window nothing is reported, which is what makes the window
    // worth passing at all.
    let elsewhere =
        p.time_by_purpose(PurposeWindow::new(date(2026, 1, 1), date(2026, 1, 2))).unwrap();
    assert!(elsewhere.iter().all(|r| r.actual_minutes == 0 && r.planned_minutes == 0));

    tasks.delete_project(project.id).unwrap();
    p.delete_goal(goal.id).unwrap();
    p.delete_goal(other.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn unattributed_time_is_reported_rather_than_dropped(store: &dyn JournalStore) {
    // Most of a life is not booked against anything, and a report that
    // quietly dropped that share would be flattering rather than useful.
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);

    let day = date(2026, 9, 15);
    let at = day.at(11, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let block = TimeBlock::new(BlockSubject::Adhoc, at, 45, "UTC").of_kind(BlockKind::Actual);
    tasks.put_block(&block).unwrap();

    let rows = p.time_by_purpose(PurposeWindow::new(day, day)).unwrap();
    let none =
        rows.iter().find(|r| r.purpose.is_none()).expect("the unattributed row is always present");
    assert_eq!(none.actual_minutes, 45);
    assert_eq!(none.blocks, 1);

    tasks.delete_block(block.id).unwrap();
}

fn clearing_a_purpose_removes_it_from_the_reports(store: &dyn JournalStore) {
    // Setting a purpose and then unsetting it must leave no trace. The
    // pointer is an index over what the sealed record says, and an index
    // that keeps rows the record no longer claims is an index that lies.
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Cleared");
    let goal = seeded_goal(store, &role, "briefly");

    let day = date(2026, 9, 16);
    let at = day.at(8, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let mut block = TimeBlock::new(BlockSubject::Adhoc, at, 20, "UTC").of_kind(BlockKind::Actual);
    block.purpose = Some(goal.purpose());
    tasks.put_block(&block).unwrap();

    let window = PurposeWindow::new(day, day);
    assert!(p.time_by_purpose(window).unwrap().iter().any(|r| r.purpose == Some(goal.purpose())));

    block.purpose = None;
    tasks.put_block(&block).unwrap();
    assert_eq!(tasks.get_block(block.id).unwrap().purpose, None);

    let rows = p.time_by_purpose(window).unwrap();
    assert!(
        !rows.iter().any(|r| r.purpose == Some(goal.purpose())),
        "an unset purpose must vanish from the report, not linger in the index"
    );
    assert!(rows.iter().any(|r| r.purpose.is_none() && r.actual_minutes == 20));

    tasks.delete_block(block.id).unwrap();
    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn deleting_a_record_takes_its_purpose_with_it(store: &dyn JournalStore) {
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Deleted");
    let goal = seeded_goal(store, &role, "doomed");

    let day = date(2026, 9, 17);
    let at = day.at(8, 0, 0, 0).in_tz("UTC").unwrap().timestamp();

    // A project with a task and a block beneath it, all attributed, then the
    // whole tree removed in one call. Nothing of it may show in the report.
    let mut project = Project::new("doomed project");
    project.purpose = Some(goal.purpose());
    tasks.put_project(&project).unwrap();
    let mut task = Task::new("doomed task");
    task.project_id = Some(project.id);
    tasks.put_task(&task).unwrap();
    let block = TimeBlock::new(BlockSubject::Task { id: task.id }, at, 90, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&block).unwrap();

    let window = PurposeWindow::new(day, day);
    assert!(p.time_by_purpose(window).unwrap().iter().any(|r| r.purpose == Some(goal.purpose())));

    tasks.delete_project(project.id).unwrap();
    let rows = p.time_by_purpose(window).unwrap();
    assert!(
        rows.iter().all(|r| r.purpose != Some(goal.purpose())),
        "a deleted tree must leave no pointer rows behind"
    );
    assert_eq!(p.goal_activity(goal.id).unwrap(), Default::default());

    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn goal_activity_counts_what_points_at_it(store: &dyn JournalStore) {
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Active");
    let goal = seeded_goal(store, &role, "measured");

    let mut project = Project::new("for the goal");
    project.purpose = Some(goal.purpose());
    tasks.put_project(&project).unwrap();

    let mut open = Task::new("still to do");
    open.project_id = Some(project.id);
    tasks.put_task(&open).unwrap();

    let mut done = Task::new("did it");
    done.project_id = Some(project.id);
    done.set_status(TaskStatus::Done);
    tasks.put_task(&done).unwrap();

    let at = date(2026, 9, 18).at(8, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let logged = TimeBlock::new(BlockSubject::Task { id: open.id }, at, 120, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&logged).unwrap();
    // Planned time is an intention and must not be counted as effort spent.
    let planned = TimeBlock::new(BlockSubject::Task { id: open.id }, at, 240, "UTC");
    tasks.put_block(&planned).unwrap();

    let activity = p.goal_activity(goal.id).unwrap();
    assert_eq!(activity.projects, 1);
    assert_eq!(activity.open_tasks, 1);
    assert_eq!(activity.done_tasks, 1);
    assert_eq!(activity.actual_minutes, 120, "only what actually happened counts as hours");
    assert!(!activity.is_empty());
    assert!(activity.last_touched.is_some(), "something happened, so there is a latest moment");

    // A goal nothing points at reports nothing rather than failing.
    let untouched = seeded_goal(store, &role, "nothing yet");
    let empty = p.goal_activity(untouched.id).unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.last_touched, None);

    tasks.delete_project(project.id).unwrap();
    p.delete_goal(goal.id).unwrap();
    p.delete_goal(untouched.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn events_are_attributed_by_their_calendar(store: &dyn JournalStore) {
    let Some(calendars) = store.calendars() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Meetings");

    let mut cal = Calendar::subscribed("work", "https://example.com/work.ics");
    cal.role_id = Some(role.id);
    calendars.put_calendar(&cal).unwrap();

    let mut unfiled = Calendar::subscribed("holidays", "https://example.com/hol.ics");
    unfiled.role_id = None;
    calendars.put_calendar(&unfiled).unwrap();

    // `sample_event` runs 09:00 to 10:00 on its first day, so each of these
    // is one hour and the two-day one is twenty-five.
    let day = date(2026, 9, 19);
    calendars.replace_events(cal.id, &[sample_event(cal.id, "purpose-1", day, day)]).unwrap();
    calendars
        .replace_events(unfiled.id, &[sample_event(unfiled.id, "purpose-2", day, day)])
        .unwrap();

    let rows = p.events_by_role(PurposeWindow::new(day, day)).unwrap();
    let mine = rows
        .iter()
        .find(|r| r.role_id == Some(role.id))
        .expect("a feed with a role reports under it");
    assert_eq!(mine.minutes, 60);
    assert_eq!(mine.events, 1);

    let theirs = rows
        .iter()
        .find(|r| r.role_id.is_none())
        .expect("a feed with no role is reported, not dropped");
    assert_eq!(theirs.minutes, 60);

    calendars.delete_calendar(cal.id).unwrap();
    calendars.delete_calendar(unfiled.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn unicode_survives_a_purpose_round_trip(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let mut role = LifeRole::new("親 · роль · 🧭");
    role.notes = "\u{1f3e1} home".into();
    p.put_role(&role).unwrap();
    assert_eq!(p.get_role(role.id).unwrap().name, "親 · роль · 🧭");

    let mut goal = Goal::new(role.id, "Viya lernt Fahrrad fahren 🚲");
    goal.notes = "auf dem Gras anfangen".into();
    p.put_goal(&goal).unwrap();
    assert_eq!(p.get_goal(goal.id).unwrap(), goal);

    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

// ---------------------------------------------------------------------------
// The tracking domain
// ---------------------------------------------------------------------------

/// The tracking half of the suite. Called by [`run_all`] when the backend
/// has a [`TrackerStore`](super::trackers::TrackerStore).
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

fn tracker_store(store: &dyn JournalStore) -> &dyn super::trackers::TrackerStore {
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
    assert!(
        matches!(t.get_tracker(TrackerId::new()), Err(crate::Error::NotFound { .. })),
        "a tracker that was never written is not found, not a default one"
    );
    assert!(matches!(t.get_reading(ReadingId::new()), Err(crate::Error::NotFound { .. })));
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

// ---- notes --------------------------------------------------------------

/// Everything a backend must do with notes.
pub fn run_note_suite(store: &dyn JournalStore) {
    eprintln!("--- note conformance suite ---");

    notes_start_empty(store);
    note_round_trips_every_field(store);
    note_put_is_idempotent(store);
    a_conditional_note_put_refuses_a_stale_write(store);
    missing_note_is_not_found(store);
    list_notes_filters_and_sorts(store);
    all_notes_carries_bodies_and_the_list_does_not(store);
    a_notes_purpose_survives_and_goes_with_it(store);
    garbage_collection_keeps_pictures_in_notes(store);
    unicode_survives_a_note_round_trip(store);

    note_cleanup(store);
    eprintln!("--- note suite passed ---");
}

fn note_store(store: &dyn JournalStore) -> &dyn super::notes::NoteStore {
    store.notes().expect("the note suite needs a note store")
}

fn note_cleanup(store: &dyn JournalStore) {
    let n = note_store(store);
    for note in n.list_notes(&NoteQuery::default()).expect("list_notes") {
        n.delete_note(note.id).expect("delete_note");
    }
    assert!(n.list_notes(&NoteQuery::default()).unwrap().is_empty(), "cleanup left notes behind");
}

fn seeded_note(store: &dyn JournalStore, title: &str, body: &str) -> Note {
    let note = Note::written(title, body);
    note_store(store).put_note(&note).expect("put_note");
    note
}

fn notes_start_empty(store: &dyn JournalStore) {
    assert!(
        note_store(store).list_notes(&NoteQuery::default()).unwrap().is_empty(),
        "a fresh store must have no notes"
    );
}

fn note_round_trips_every_field(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut note = Note::written("Sailing", "Reach, run, beat.");
    note.tags = vec!["boats".into(), "summer".into()];
    note.pinned = true;
    n.put_note(&note).expect("put_note");

    let back = n.get_note(note.id).expect("get_note");
    assert_eq!(back, note, "every field must survive the round trip");

    let listed = n.list_notes(&NoteQuery::default()).expect("list_notes");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Sailing");
    assert_eq!(listed[0].tags, vec!["boats".to_string(), "summer".to_string()]);
    assert!(listed[0].pinned);

    n.delete_note(note.id).expect("delete_note");
    assert!(n.list_notes(&NoteQuery::default()).unwrap().is_empty());
}

fn note_put_is_idempotent(store: &dyn JournalStore) {
    let n = note_store(store);
    let note = Note::written("Twice", "once");
    n.put_note(&note).expect("first put");
    n.put_note(&note).expect("second put");
    assert_eq!(n.list_notes(&NoteQuery::default()).unwrap().len(), 1, "saving twice leaves one");

    // Deleting something that is not there is not an error, so a client that
    // retries a delete over a dropped connection is not told it failed.
    n.delete_note(note.id).expect("delete_note");
    n.delete_note(note.id).expect("deleting a missing note is a no-op");
}

fn a_conditional_note_put_refuses_a_stale_write(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut note = Note::written("Shared", "first");
    n.put_note_if(&note, None).expect("creating with no expectation");

    // Creating something that is already there is a conflict, not an
    // overwrite: two windows both minting the same note is the case.
    assert_eq!(
        n.put_note_if(&note, None).unwrap_err().code(),
        "conflict",
        "creating over an existing note must be refused"
    );

    let seen = note.updated_at;
    note.title = "Shared, edited".into();
    note.updated_at = Timestamp::now();
    n.put_note_if(&note, Some(seen)).expect("writing over the version we read");

    // The stale expectation is what a second window holds.
    let mut stale = note.clone();
    stale.title = "Someone else's edit".into();
    assert_eq!(
        n.put_note_if(&stale, Some(seen)).unwrap_err().code(),
        "conflict",
        "a write over a version that has moved on must be refused"
    );

    // And a note that has been deleted under a caller who thought they were
    // updating it is a conflict too, not a silent resurrection.
    n.delete_note(note.id).expect("delete_note");
    assert_eq!(
        n.put_note_if(&note, Some(note.updated_at)).unwrap_err().code(),
        "conflict",
        "updating a note that has been deleted must be refused"
    );
    note_cleanup(store);
}

fn missing_note_is_not_found(store: &dyn JournalStore) {
    let err = note_store(store).get_note(NoteId::new()).unwrap_err();
    assert_eq!(err.code(), "not_found", "a note that is not there is not found");
}

fn list_notes_filters_and_sorts(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut alpha = Note::written("Alpha", "first");
    alpha.tags = vec!["Boats".into()];
    let mut zeta = Note::written("Zeta", "second");
    zeta.tags = vec!["boats".into(), "summer".into()];
    zeta.pinned = true;
    n.put_note(&alpha).expect("put alpha");
    n.put_note(&zeta).expect("put zeta");

    let by_title = n
        .list_notes(&NoteQuery { sort: NoteSort::TitleAsc, ..Default::default() })
        .expect("list by title");
    assert_eq!(
        by_title.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(),
        ["Zeta", "Alpha"],
        "a pin outranks the alphabet"
    );

    // Case must not decide whether a tag matches: "Boats" and "boats" are
    // one tag to a person.
    let tagged = n
        .list_notes(&NoteQuery { tags: vec!["boats".into()], ..Default::default() })
        .expect("list by tag");
    assert_eq!(tagged.len(), 2);

    let both = n
        .list_notes(&NoteQuery {
            tags: vec!["boats".into(), "summer".into()],
            ..Default::default()
        })
        .expect("list by two tags");
    assert_eq!(both.len(), 1, "every listed tag has to be there, not any of them");

    let pinned =
        n.list_notes(&NoteQuery { pinned: Some(true), ..Default::default() }).expect("list pinned");
    assert_eq!(pinned.len(), 1);

    let capped = n
        .list_notes(&NoteQuery { limit: Some(1), ..Default::default() })
        .expect("list with a limit");
    assert_eq!(capped.len(), 1);

    note_cleanup(store);
}

fn all_notes_carries_bodies_and_the_list_does_not(store: &dyn JournalStore) {
    let n = note_store(store);
    let note = seeded_note(store, "Recipe", "Two hundred grams of flour.");

    let listed = n.list_notes(&NoteQuery::default()).expect("list_notes");
    assert_eq!(listed[0].excerpt, "Two hundred grams of flour.");

    let all = n.all_notes().expect("all_notes");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].body.plain_text().trim(), "Two hundred grams of flour.");
    assert_eq!(all[0].id, note.id);

    let tags = n.note_tags().expect("note_tags");
    assert!(tags.is_empty(), "an untagged note contributes no tags");

    note_cleanup(store);
}

fn a_notes_purpose_survives_and_goes_with_it(store: &dyn JournalStore) {
    let Some(p) = store.purpose() else {
        eprintln!("  (no purpose store; skipping the note's purpose)");
        return;
    };
    let role = LifeRole::new("Sailor");
    p.put_role(&role).expect("put_role");

    let n = note_store(store);
    let mut note = Note::written("Log", "Wind from the south-west.");
    note.purpose = Some(Purpose::Role { id: role.id });
    n.put_note(&note).expect("put_note");

    assert_eq!(
        n.get_note(note.id).unwrap().purpose,
        Some(Purpose::Role { id: role.id }),
        "a note's purpose must survive the round trip"
    );
    assert_eq!(
        n.list_notes(&NoteQuery::default()).unwrap()[0].purpose,
        Some(Purpose::Role { id: role.id }),
        "and be on the summary, so a list can say what a note is filed under"
    );

    // The side table is an index over the payload, and deleting the record
    // it indexes must take the row with it.
    n.delete_note(note.id).expect("delete_note");
    let mut revived = note.clone();
    revived.purpose = None;
    n.put_note(&revived).expect("put_note");
    assert_eq!(
        n.get_note(note.id).unwrap().purpose,
        None,
        "a re-made note must not inherit the deleted one's filing"
    );

    n.delete_note(note.id).expect("delete_note");
    p.delete_role(role.id).expect("delete_role");
}

fn garbage_collection_keeps_pictures_in_notes(store: &dyn JournalStore) {
    if !store.capabilities().blobs {
        eprintln!("  (no blobs; skipping the note's pictures)");
        return;
    }
    let blob = store.put_blob(b"a photograph in a note").expect("put_blob");
    let mut note = Note::new("Illustrated");
    note.body = RichDoc(json!({
        "type": "doc",
        "content": [{"type": MEDIA_NODE, "attrs": {"blob": blob.to_hex(), "kind": "image"}}]
    }));
    note_store(store).put_note(&note).expect("put_note");

    store.collect_garbage(std::time::Duration::ZERO).expect("collect_garbage");
    assert!(
        store.has_blob(blob).unwrap(),
        "a picture in a note is referenced, and must survive a sweep"
    );

    note_store(store).delete_note(note.id).expect("delete_note");
    store.collect_garbage(std::time::Duration::ZERO).expect("collect_garbage");
    assert!(!store.has_blob(blob).unwrap(), "and must be swept once nothing points at it");
}

fn unicode_survives_a_note_round_trip(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut note = Note::written("மீன் \u{1f41f}", "நீரில் நீந்துகிறது");
    note.tags = vec!["தமிழ்".into()];
    n.put_note(&note).expect("put_note");
    let back = n.get_note(note.id).expect("get_note");
    assert_eq!(back.title, "மீன் \u{1f41f}");
    assert_eq!(back.tags, vec!["தமிழ்".to_string()]);
    assert_eq!(back.body.plain_text().trim(), "நீரில் நீந்துகிறது");
    n.delete_note(note.id).expect("delete_note");
}

/// The one row that says whose vault this is.
///
/// Part of the base battery rather than an optional suite, because every
/// backend must answer it: the default on the trait says "nothing filled in",
/// which is a real answer, and a backend that cannot store one has to say so
/// rather than silently forgetting what was typed.
fn the_owner_round_trips_and_starts_empty(store: &dyn JournalStore) {
    let blank = store.profile().expect("a fresh store still has an answer");
    assert!(blank.is_empty(), "nobody has said who they are yet");

    let mine = Profile {
        first_name: "Hari".into(),
        last_name: "Govardhanam".into(),
        born: Some(date(1985, 3, 14)),
        gender: "male".into(),
        location: "Seattle".into(),
        about: "Software, two children, sailing at weekends".into(),
        updated_at: Some(Timestamp::now()),
    };
    match store.put_profile(&mine) {
        Ok(()) => {}
        // A backend written before this existed says so rather than pretending.
        Err(e) if e.code() == "unsupported" => {
            eprintln!("  (backend stores no profile; skipping the owner)");
            return;
        }
        Err(e) => panic!("put_profile: {e}"),
    }

    assert_eq!(store.profile().unwrap(), mine, "every field must survive the round trip");

    // Idempotent, and a second write replaces rather than adding a row.
    store.put_profile(&mine).expect("put_profile again");
    assert_eq!(store.profile().unwrap(), mine);

    // Emptied by hand is emptied, not reverted to what was there before.
    store.put_profile(&Profile::default()).expect("clearing the profile");
    assert!(store.profile().unwrap().is_empty(), "clearing it must actually clear it");
}

// ---- routines -----------------------------------------------------------

/// Everything a backend must do with the assistant's standing work.
pub fn run_routine_suite(store: &dyn JournalStore) {
    eprintln!("--- routine conformance suite ---");

    routines_start_empty(store);
    routine_round_trips_every_field(store);
    routine_put_is_idempotent(store);
    missing_routine_records_are_not_found(store);
    runs_are_listed_newest_first_and_filtered(store);
    unseen_runs_are_counted_and_cleared(store);
    deleting_a_routine_takes_its_runs(store);
    a_runs_transcript_is_hidden_from_the_chat_list(store);
    unicode_survives_a_routine_round_trip(store);

    routine_cleanup(store);
    eprintln!("--- routine suite passed ---");
}

fn routine_store(store: &dyn JournalStore) -> &dyn super::routines::RoutineStore {
    store.routines().expect("the routine suite needs a routine store")
}

fn routine_cleanup(store: &dyn JournalStore) {
    let r = routine_store(store);
    for routine in r.list_routines().expect("list_routines") {
        r.delete_routine(routine.id).expect("delete_routine");
    }
    for run in r.list_runs(&RunQuery::default()).expect("list_runs") {
        r.delete_run(run.id).expect("delete_run");
    }
    assert!(r.list_routines().unwrap().is_empty(), "cleanup left routines behind");
    assert!(r.list_runs(&RunQuery::default()).unwrap().is_empty(), "cleanup left runs behind");
}

fn seeded_routine(store: &dyn JournalStore, name: &str) -> Routine {
    let routine = Routine::new(name, "Say what is due today.", Trigger::Manual);
    routine_store(store).put_routine(&routine).expect("put_routine");
    routine
}

fn routines_start_empty(store: &dyn JournalStore) {
    let r = routine_store(store);
    assert!(r.list_routines().unwrap().is_empty(), "a fresh store has no routines");
    assert!(r.list_runs(&RunQuery::default()).unwrap().is_empty(), "and no runs");
    assert_eq!(r.count_unseen_runs().unwrap(), 0);
}

fn routine_round_trips_every_field(store: &dyn JournalStore) {
    let r = routine_store(store);
    let mut routine = Routine::new(
        "Morning brief",
        "Look at what is due and leave me a note.",
        Trigger::Schedule { at: time(7, 0, 0, 0), days: everyday_weekdays() },
    );
    routine.grace_minutes = 90;
    routine.last_run_at = Some(Timestamp::now());
    r.put_routine(&routine).expect("put_routine");

    assert_eq!(r.get_routine(routine.id).unwrap(), routine, "every field must survive");
    assert_eq!(r.list_routines().unwrap().len(), 1);

    let mut run = RoutineRun::new(&routine, Some(Timestamp::now()));
    run.subject = Some("the 3pm with Priya".into());
    run.summary = "Three things are due and one is overdue.".into();
    run.steps = 4;
    run.finish(Outcome::Done, run.summary.clone());
    r.put_run(&run).expect("put_run");
    assert_eq!(r.get_run(run.id).unwrap(), run, "and every field of a run");

    routine_cleanup(store);
}

fn routine_put_is_idempotent(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Twice");
    r.put_routine(&routine).expect("second put");
    assert_eq!(r.list_routines().unwrap().len(), 1, "saving twice leaves one");

    r.delete_routine(routine.id).expect("delete_routine");
    r.delete_routine(routine.id).expect("deleting a missing routine is a no-op");
    routine_cleanup(store);
}

fn missing_routine_records_are_not_found(store: &dyn JournalStore) {
    let r = routine_store(store);
    assert_eq!(r.get_routine(RoutineId::new()).unwrap_err().code(), "not_found");
    assert_eq!(r.get_run(RoutineRunId::new()).unwrap_err().code(), "not_found");
    r.delete_run(RoutineRunId::new()).expect("deleting a missing run is a no-op");
}

fn runs_are_listed_newest_first_and_filtered(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Brief");
    let other = seeded_routine(store, "Review");

    let mut first = RoutineRun::new(&routine, None);
    first.started_at = Timestamp::from_second(1_700_000_000).unwrap();
    first.finish(Outcome::Done, "the first");
    let mut second = RoutineRun::new(&routine, None);
    second.started_at = Timestamp::from_second(1_700_001_000).unwrap();
    second.fail("the endpoint refused");
    let mut elsewhere = RoutineRun::new(&other, None);
    elsewhere.started_at = Timestamp::from_second(1_700_002_000).unwrap();
    elsewhere.finish(Outcome::Done, "somebody else's");
    for run in [&first, &second, &elsewhere] {
        r.put_run(run).expect("put_run");
    }

    let all = r.list_runs(&RunQuery::default()).expect("list_runs");
    assert_eq!(
        all.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![elsewhere.id, second.id, first.id],
        "newest first"
    );

    let mine = r.list_runs(&RunQuery::for_routine(routine.id)).expect("one routine's log");
    assert_eq!(mine.len(), 2, "and only that routine's");

    let failed = r
        .list_runs(&RunQuery { outcomes: vec![Outcome::Failed], ..Default::default() })
        .expect("by outcome");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].reason, "the endpoint refused");

    let capped =
        r.list_runs(&RunQuery { limit: Some(1), ..Default::default() }).expect("with a limit");
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].id, elsewhere.id, "a limit keeps the newest, not any one");

    let recent = r
        .list_runs(&RunQuery {
            since: Some(Timestamp::from_second(1_700_001_500).unwrap()),
            ..Default::default()
        })
        .expect("since");
    assert_eq!(recent.len(), 1);

    routine_cleanup(store);
}

fn unseen_runs_are_counted_and_cleared(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Brief");
    let mut one = RoutineRun::new(&routine, None);
    one.finish(Outcome::Done, "first");
    let mut two = RoutineRun::new(&routine, None);
    two.finish(Outcome::Done, "second");
    r.put_run(&one).expect("put_run");
    r.put_run(&two).expect("put_run");
    assert_eq!(r.count_unseen_runs().unwrap(), 2);

    r.mark_runs_seen(&[one.id]).expect("mark one");
    assert_eq!(r.count_unseen_runs().unwrap(), 1);
    // Both copies of the flag have to move: it is a clear column *and* part of
    // the sealed payload, and a client reading the record back would otherwise
    // put the number straight back on the app bar.
    assert!(r.get_run(one.id).unwrap().seen, "the payload must agree with the column");
    assert_eq!(r.list_runs(&RunQuery::unseen(10)).unwrap().len(), 1);

    r.mark_runs_seen(&[]).expect("mark all");
    assert_eq!(r.count_unseen_runs().unwrap(), 0);
    assert!(r.get_run(two.id).unwrap().seen);

    // A skipped run is born seen: there is nothing to look at, so it must not
    // put a number on the app bar.
    let skipped = RoutineRun::skipped(&routine, None, "the vault was locked");
    r.put_run(&skipped).expect("put_run");
    assert_eq!(r.count_unseen_runs().unwrap(), 0, "a skipped run asks for no attention");

    routine_cleanup(store);
}

fn deleting_a_routine_takes_its_runs(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Brief");
    let other = seeded_routine(store, "Review");
    let mut mine = RoutineRun::new(&routine, None);
    mine.finish(Outcome::Done, "mine");
    let mut theirs = RoutineRun::new(&other, None);
    theirs.finish(Outcome::Done, "theirs");
    r.put_run(&mine).expect("put_run");
    r.put_run(&theirs).expect("put_run");

    r.delete_routine(routine.id).expect("delete_routine");
    assert_eq!(r.get_run(mine.id).unwrap_err().code(), "not_found", "its runs go with it");
    assert!(r.get_run(theirs.id).is_ok(), "and nobody else's do");

    routine_cleanup(store);
}

fn a_runs_transcript_is_hidden_from_the_chat_list(store: &dyn JournalStore) {
    let Some(agent) = store.agent() else {
        eprintln!("  (no agent store; skipping the run's transcript)");
        return;
    };
    let chat = Conversation::new();
    let transcript = Conversation::for_run(RoutineRunId::new(), "Morning brief");
    agent.put_conversation(&chat).expect("put_conversation");
    agent.put_conversation(&transcript).expect("put_conversation");

    let everything = agent.list_conversations(&ConversationQuery::default()).expect("all threads");
    assert_eq!(everything.len(), 2, "both are threads");

    let chats = agent.list_conversations(&ConversationQuery::chats(10)).expect("chats only");
    assert_eq!(chats.len(), 1, "a week of morning briefs is not a list of conversations");
    assert_eq!(chats[0].id, chat.id);

    // And the limit is honoured *after* the filter, not before: a `LIMIT 1`
    // pushed into SQL would have fetched the transcript and answered with
    // nothing at all.
    let one = agent.list_conversations(&ConversationQuery::chats(1)).expect("one chat");
    assert_eq!(one.len(), 1, "asking for one chat must give one chat");
    assert_eq!(one[0].id, chat.id);

    agent.delete_conversation(chat.id).expect("delete_conversation");
    agent.delete_conversation(transcript.id).expect("delete_conversation");
}

fn unicode_survives_a_routine_round_trip(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine =
        Routine::new("காலை அறிக்கை \u{2600}", "இன்று என்ன செய்ய வேண்டும் என்று சொல்", Trigger::Manual);
    r.put_routine(&routine).expect("put_routine");
    let back = r.get_routine(routine.id).expect("get_routine");
    assert_eq!(back.name, "காலை அறிக்கை \u{2600}");
    assert_eq!(back.instructions, routine.instructions);
    r.delete_routine(routine.id).expect("delete_routine");
}

/// Monday to Friday, spelled out so the suite does not depend on a constant
/// that could quietly change meaning.
fn everyday_weekdays() -> Vec<crate::routine::Weekday> {
    use crate::routine::Weekday as W;
    vec![W::Mon, W::Tue, W::Wed, W::Thu, W::Fri]
}

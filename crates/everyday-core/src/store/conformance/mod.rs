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
//!
//! # Where the code is
//!
//! One file per domain, the same split `everyday_core::store` itself makes
//! between [`JournalStore`] and its optional siblings. `journal.rs` is the
//! base battery -- journals, entries, blobs, stats, the profile row -- and is
//! not gated behind an accessor because every backend has one. Each of the
//! others is run by [`run_all`] only when the backend's own accessor answers,
//! and is public so a backend under construction can run one suite alone
//! while the rest of it is still unwritten.

mod agent;
mod calendars;
mod journal;
mod library;
mod notes;
mod purpose;
mod routines;
mod tasks;
mod trackers;

pub use agent::run_agent_suite;
pub use calendars::run_calendar_suite;
pub use library::run_library_suite;
pub use notes::run_note_suite;
pub use purpose::run_purpose_suite;
pub use routines::run_routine_suite;
pub use tasks::run_task_suite;
pub use trackers::run_tracker_suite;

use super::agent::{AgentStore, ConversationQuery};
use super::calendars::{CalendarStore, EventQuery};
use super::library::{ItemQuery, ItemSort, LibraryStore, LogQuery};
use super::purpose::{GoalQuery, PurposeWindow};
use super::tasks::{BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore};
use super::trackers::ReadingQuery;
use super::{EntryQuery, JournalStore, SortOrder};
use crate::agent::{AgentSettings, Conversation, LLMModelConfig, Memory, Message, Role, ToolCall};
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

    journal::run_journal_suite(store);

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
            library::garbage_collection_keeps_library_covers(store);
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

    journal::cleanup(store);
    eprintln!("--- backend {name:?} passed ---");
}

/// The one way every suite checks "this id is not there".
///
/// Call sites used to spell it two different ways -- `.code() ==
/// "not_found"` in some, `matches!(err, Error::NotFound { .. })` in others --
/// which say the same thing and read as though they might not. This says it
/// once, and takes the whole `Result` so a call site is one line rather than
/// a `let` binding followed by an assertion.
pub(super) fn assert_not_found<T: std::fmt::Debug>(result: crate::error::Result<T>) {
    let err = result.expect_err("expected a not-found error");
    assert_eq!(err.code(), "not_found", "expected not_found, got {err:?}");
}

//! The database schema, and the migrations that reach it.
//!
//! Kept apart from the query code so that "what shape is the database" is one
//! file you can read start to finish, rather than three constants buried
//! between two hundred lines of `SELECT`. Each domain was added by a numbered
//! step that leaves the ones before it alone -- see [`migrate`].

use everyday_core::error::{Error, Result};
use rusqlite::Connection;

/// Schema the code in this crate expects. Bumped by adding a step below.
pub(crate) const SCHEMA_VERSION: i64 = 6;

/// Bring the database up to [`SCHEMA_VERSION`].
///
/// Stepped rather than all-or-nothing: a vault written by an earlier build
/// has entries in it, so version 2 must *add* the task tables beside them
/// rather than recreate the file, version 3 the calendar tables beside both,
/// version 4 the library tables beside all three, version 5 the
/// readings beside all four, and version 6 the assistant's own beside all
/// five. Each step is
/// idempotent and runs in its own transaction, and `user_version` is only
/// advanced once they all land.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 =
        conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(Error::backend)?;
    // A database from a *newer* build is refused rather than opened.
    //
    // Nothing here can know what a later version did to the schema, and the
    // failure is silent in the worst way: the steps below are all skipped,
    // every query still parses against whatever columns happen to remain,
    // and writes land in a shape the newer build did not expect. `Vault::open`
    // already makes exactly this check against the header's format version;
    // the store had been the one layer that would open anything.
    if version > SCHEMA_VERSION {
        return Err(Error::UnsupportedVaultVersion {
            found: version as u32,
            supported: SCHEMA_VERSION as u32,
        });
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version < 1 {
        conn.execute_batch(SCHEMA_V1).map_err(Error::backend)?;
    }
    if version < 2 {
        conn.execute_batch(SCHEMA_V2).map_err(Error::backend)?;
    }
    if version < 3 {
        conn.execute_batch(SCHEMA_V3).map_err(Error::backend)?;
    }
    if version < 4 {
        conn.execute_batch(SCHEMA_V4).map_err(Error::backend)?;
    }
    if version < 5 {
        conn.execute_batch(SCHEMA_V5).map_err(Error::backend)?;
    }
    if version < 6 {
        conn.execute_batch(SCHEMA_V6).map_err(Error::backend)?;
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(Error::backend)?;
    Ok(())
}

/// Version 1: the journal domain.
const SCHEMA_V1: &str = r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS journals (
            id          TEXT    PRIMARY KEY NOT NULL,
            sort_order  INTEGER NOT NULL DEFAULT 0,
            updated_us  INTEGER NOT NULL,
            data        BLOB    NOT NULL
        );

        CREATE TABLE IF NOT EXISTS entries (
            id          TEXT    PRIMARY KEY NOT NULL,
            journal_id  TEXT    NOT NULL,
            local_date  TEXT    NOT NULL,
            created_us  INTEGER NOT NULL,
            updated_us  INTEGER NOT NULL,
            starred     INTEGER NOT NULL DEFAULT 0,
            pinned      INTEGER NOT NULL DEFAULT 0,
            data        BLOB    NOT NULL,
            summary     BLOB    NOT NULL
        );

        -- The list view is always "this journal, newest first", so the
        -- covering index is (journal_id, local_date, created_us).
        CREATE INDEX IF NOT EXISTS entries_by_journal_date
            ON entries (journal_id, local_date DESC, created_us DESC);
        CREATE INDEX IF NOT EXISTS entries_by_date
            ON entries (local_date DESC, created_us DESC);
        CREATE INDEX IF NOT EXISTS entries_by_updated
            ON entries (updated_us DESC);
        COMMIT;
        "#;

/// Version 2: the task domain -- projects, tasks and time blocks.
///
/// No foreign keys between the three, deliberately. `project_id` and
/// `parent_id` are plain columns because the cascades this domain wants are
/// not the ones `ON DELETE CASCADE` gives: deleting a task must also take
/// the *time blocks* pointing at it, which is a rule about a table SQLite
/// cannot see the link to (the subject is inside the sealed payload; the
/// columns beside it are a denormalised copy for the indexes). Keeping the
/// whole cascade in one place in Rust is what stops half of it drifting.
const SCHEMA_V2: &str = r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS projects (
            id           TEXT    PRIMARY KEY NOT NULL,
            status       TEXT    NOT NULL,
            priority     INTEGER NOT NULL DEFAULT 0,
            due_date     TEXT,
            sort_order   INTEGER NOT NULL DEFAULT 0,
            created_us   INTEGER NOT NULL,
            updated_us   INTEGER NOT NULL,
            completed_us INTEGER,
            data         BLOB    NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id           TEXT    PRIMARY KEY NOT NULL,
            project_id   TEXT,
            parent_id    TEXT,
            status       TEXT    NOT NULL,
            priority     INTEGER NOT NULL DEFAULT 0,
            start_date   TEXT,
            due_date     TEXT,
            sort_order   INTEGER NOT NULL DEFAULT 0,
            created_us   INTEGER NOT NULL,
            updated_us   INTEGER NOT NULL,
            completed_us INTEGER,
            data         BLOB    NOT NULL
        );

        -- A board draws one project's top-level cards, split by status and
        -- in manual order: that is this index, exactly.
        CREATE INDEX IF NOT EXISTS tasks_by_project
            ON tasks (project_id, status, sort_order);
        -- Expanding a task to show its subtasks.
        CREATE INDEX IF NOT EXISTS tasks_by_parent
            ON tasks (parent_id, sort_order);
        -- "What is due this week", across every project.
        CREATE INDEX IF NOT EXISTS tasks_by_due
            ON tasks (due_date, status);

        CREATE TABLE IF NOT EXISTS time_blocks (
            id         TEXT    PRIMARY KEY NOT NULL,
            task_id    TEXT,
            project_id TEXT,
            local_date TEXT    NOT NULL,
            start_us   INTEGER NOT NULL,
            end_us     INTEGER NOT NULL,
            kind       TEXT    NOT NULL,
            data       BLOB    NOT NULL
        );

        -- The calendar query: one week, in time order.
        CREATE INDEX IF NOT EXISTS blocks_by_date
            ON time_blocks (local_date, start_us);
        -- "How long did this actually take?"
        CREATE INDEX IF NOT EXISTS blocks_by_task
            ON time_blocks (task_id, kind);
        COMMIT;
        "#;

/// Version 3: the calendar domain -- subscriptions and their events.
///
/// `events` *does* carry a foreign key, unlike the task tables, and for the
/// reason those do not: here the cascade SQLite offers is exactly the
/// cascade wanted. Unsubscribing takes the feed's events and nothing else,
/// because nothing else in the vault points at an event -- they are a cache
/// of what a server said, not a record anyone linked to.
///
/// Two date columns rather than one. A week query has to find a fortnight in
/// Lisbon while standing in the middle of it, so the window test is an
/// overlap -- `end_date >= from AND local_date <= to` -- and both sides of it
/// need an index to sit on.
const SCHEMA_V3: &str = r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS calendars (
            id          TEXT    PRIMARY KEY NOT NULL,
            visible     INTEGER NOT NULL DEFAULT 1,
            created_us  INTEGER NOT NULL,
            updated_us  INTEGER NOT NULL,
            synced_us   INTEGER,
            data        BLOB    NOT NULL
        );

        CREATE TABLE IF NOT EXISTS events (
            id          TEXT    PRIMARY KEY NOT NULL,
            calendar_id TEXT    NOT NULL
                                REFERENCES calendars (id) ON DELETE CASCADE,
            local_date  TEXT    NOT NULL,
            end_date    TEXT    NOT NULL,
            start_us    INTEGER NOT NULL,
            end_us      INTEGER NOT NULL,
            all_day     INTEGER NOT NULL DEFAULT 0,
            data        BLOB    NOT NULL
        );

        -- The calendar query: everything touching a window, in time order.
        CREATE INDEX IF NOT EXISTS events_by_window
            ON events (end_date, local_date, start_us);
        -- A sync replaces one feed's rows, and the sidebar counts them.
        CREATE INDEX IF NOT EXISTS events_by_calendar
            ON events (calendar_id);
        COMMIT;
        "#;

/// Version 4: the library domain -- shelves, the things on them, and the log.
///
/// Foreign keys, as the calendar tables have and the task tables do not, and
/// for the calendar's reason: here the cascade SQLite offers is exactly the
/// cascade wanted, and it reaches nothing outside these three tables.
/// Deleting a shelf takes its items, deleting an item takes its log, and
/// nothing else in the vault points at either. What is deliberately *not*
/// cascaded is a cover: covers are content-addressed blobs, shared between
/// records, and are reclaimed by the ordinary garbage collector on its grace
/// period rather than deleted by whoever dropped the last reference.
///
/// Note what stays in the clear, which is only what an index is built from:
/// `kind_id`, `status`, `rating`, `favourite`, `finished_on`, `year`, and a
/// log row's `date` and `event`. Titles, creators, notes, tags, summaries,
/// addresses, cover ids and the *names of the shelves themselves* are all
/// sealed -- so the database says that somebody rated eleven things highly
/// in March and never what any of them were.
const SCHEMA_V4: &str = r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS kinds (
            id          TEXT    PRIMARY KEY NOT NULL,
            sort_order  INTEGER NOT NULL DEFAULT 0,
            visible     INTEGER NOT NULL DEFAULT 1,
            created_us  INTEGER NOT NULL,
            updated_us  INTEGER NOT NULL,
            data        BLOB    NOT NULL
        );

        CREATE TABLE IF NOT EXISTS items (
            id           TEXT    PRIMARY KEY NOT NULL,
            kind_id      TEXT    NOT NULL
                                 REFERENCES kinds (id) ON DELETE CASCADE,
            status       TEXT    NOT NULL,
            rating       INTEGER,
            favourite    INTEGER NOT NULL DEFAULT 0,
            year         INTEGER,
            started_on   TEXT,
            finished_on  TEXT,
            sort_order   INTEGER NOT NULL DEFAULT 0,
            created_us   INTEGER NOT NULL,
            updated_us   INTEGER NOT NULL,
            data         BLOB    NOT NULL
        );

        -- A shelf draws one kind, split by status, in manual order: that is
        -- this index, exactly.
        CREATE INDEX IF NOT EXISTS items_by_kind
            ON items (kind_id, status, sort_order);
        -- "What did I get through this year", across every shelf.
        CREATE INDEX IF NOT EXISTS items_by_finished
            ON items (finished_on);
        CREATE INDEX IF NOT EXISTS items_by_updated
            ON items (updated_us DESC);

        CREATE TABLE IF NOT EXISTS logs (
            id          TEXT    PRIMARY KEY NOT NULL,
            item_id     TEXT    NOT NULL
                                REFERENCES items (id) ON DELETE CASCADE,
            event       TEXT    NOT NULL,
            local_date  TEXT    NOT NULL,
            created_us  INTEGER NOT NULL,
            data        BLOB    NOT NULL
        );

        -- One item's history, newest first.
        CREATE INDEX IF NOT EXISTS logs_by_item
            ON logs (item_id, local_date DESC);
        -- The year in review: every completion in a window.
        CREATE INDEX IF NOT EXISTS logs_by_date
            ON logs (local_date, event);
        COMMIT;
        "#;

/// Version 5: the tracking domain -- the readings a journal's trackers made.
///
/// One table, and no table for the trackers themselves: a
/// `Tracker` is a setting of a journal and is saved inside that journal's
/// sealed payload, which is what keeps "sertraline" out of the database and
/// out of this schema. What lands here is the stream: which tracker, which
/// day, at what time, how much.
///
/// `value` is a clear `REAL` column, and that is the whole point of the
/// design. A year of readings is thousands of rows whose entire purpose is
/// to be summed, averaged and counted; sealing the number would make every
/// chart a full decrypt of the vault. What the column leaks is that tracker
/// `7f3a...` was `500` at 08:12 -- never that `7f3a...` is a drug, because
/// the name it maps to is sealed one table over.
///
/// `at_us` is nullable, and deliberately so. A reading always knows its day
/// and only sometimes its minute: ticking "flossed" while writing up
/// yesterday says something true about yesterday and nothing about 23:04.
/// A defaulted timestamp there would put a pin on the calendar at an hour
/// nothing happened and skew the first question anyone asks of this data --
/// *when* do the migraines start -- so the unknown is stored as an unknown
/// and `WHERE at_us IS NOT NULL` is how an hour-of-day query says what it
/// means.
///
/// Four indexes, one per question actually asked:
///
/// | Index | Answers |
/// |---|---|
/// | `readings_by_tracker` | "this tracker, over this year" -- every chart |
/// | `readings_by_day` | "everything on these seven days" -- the calendar |
/// | `readings_by_journal` | "this journal, today" -- the chips under an entry |
/// | `readings_by_entry` | detaching readings from an entry being deleted |
const SCHEMA_V5: &str = r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS readings (
            id          TEXT    PRIMARY KEY NOT NULL,
            journal_id  TEXT    NOT NULL,
            tracker_id  TEXT    NOT NULL,
            entry_id    TEXT,
            local_date  TEXT    NOT NULL,
            at_us       INTEGER,
            value       REAL    NOT NULL,
            created_us  INTEGER NOT NULL,
            updated_us  INTEGER NOT NULL,
            data        BLOB    NOT NULL
        );

        CREATE INDEX IF NOT EXISTS readings_by_tracker
            ON readings (tracker_id, local_date, at_us);
        CREATE INDEX IF NOT EXISTS readings_by_day
            ON readings (local_date, at_us);
        CREATE INDEX IF NOT EXISTS readings_by_journal
            ON readings (journal_id, local_date);
        CREATE INDEX IF NOT EXISTS readings_by_entry
            ON readings (entry_id);
        COMMIT;
        "#;

/// Version 6: the assistant -- its configuration, its threads, and what it
/// was asked to remember.
///
/// The clear/sealed split every other version of this schema makes does not
/// appear here, and its absence is the design. Versions 4 and 5 keep
/// `status`, `rating`, `local_date` and `value` readable because an index on
/// them is what makes a shelf or a chart one query instead of a full
/// decrypt. Nothing in this domain is ever asked that sort of question: the
/// panel wants one thread in order and the history list wants the newest
/// threads, and `conversation_id` plus a timestamp answer both. So there is
/// no reason to leave a single word of a conversation outside the envelope,
/// and none is left. The file records that fourteen messages were exchanged
/// on the 3rd and nothing at all about them.
///
/// Two singleton tables, each pinned to one row by a `CHECK`. `agent_secret`
/// is separate from `agent_settings` rather than a column in it so that
/// reading the configuration -- which happens on every panel open -- cannot
/// pick up the API key on the way past. They are sealed under different
/// associated data for the same reason, so neither can be substituted for
/// the other by anyone who can write to this file.
///
/// `messages` cascades from `conversations`, as `events` does from
/// `calendars` and for the same reason: deleting a thread takes its turns
/// and nothing else, because nothing else in the vault points at a message.
/// What deliberately does *not* cascade is `memories`. A memory holds a
/// `source_id` naming the conversation that taught it, and that pointer is
/// allowed to dangle -- clearing your chat history must not silently make
/// the assistant forget that you plan on Sundays. Which is also why
/// `source_id` is not a column here at all: it lives inside the sealed
/// payload, where a foreign key cannot reach it and try to enforce the
/// cascade this domain does not want.
const SCHEMA_V6: &str = r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS agent_settings (
            id          INTEGER PRIMARY KEY CHECK (id = 1),
            data        BLOB    NOT NULL
        );

        CREATE TABLE IF NOT EXISTS agent_secret (
            id          INTEGER PRIMARY KEY CHECK (id = 1),
            data        BLOB    NOT NULL
        );

        CREATE TABLE IF NOT EXISTS conversations (
            id          TEXT    PRIMARY KEY NOT NULL,
            created_us  INTEGER NOT NULL,
            updated_us  INTEGER NOT NULL,
            data        BLOB    NOT NULL
        );

        -- The history pane: most recently used first.
        CREATE INDEX IF NOT EXISTS conversations_by_updated
            ON conversations (updated_us DESC);

        CREATE TABLE IF NOT EXISTS messages (
            id              TEXT    PRIMARY KEY NOT NULL,
            conversation_id TEXT    NOT NULL
                                    REFERENCES conversations (id) ON DELETE CASCADE,
            created_us      INTEGER NOT NULL,
            data            BLOB    NOT NULL
        );

        -- One thread, in order. `id` breaks ties because a tool result and
        -- the turn that asked for it can land in the same microsecond, and
        -- replaying them the wrong way round makes a model re-run the call.
        -- Ids are UUIDv7, so ordering by one orders by time anyway.
        CREATE INDEX IF NOT EXISTS messages_by_conversation
            ON messages (conversation_id, created_us, id);

        CREATE TABLE IF NOT EXISTS memories (
            id          TEXT    PRIMARY KEY NOT NULL,
            created_us  INTEGER NOT NULL,
            data        BLOB    NOT NULL
        );

        CREATE INDEX IF NOT EXISTS memories_by_created
            ON memories (created_us);
        COMMIT;
        "#;

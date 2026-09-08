//! The database schema, and the migrations that reach it.
//!
//! Kept apart from the query code so that "what shape is the database" is one
//! file you can read start to finish, rather than three constants buried
//! between two hundred lines of `SELECT`. Each domain was added by a numbered
//! step that leaves the ones before it alone -- see [`migrate`].

use everyday_core::error::{Error, Result};
use rusqlite::Connection;

/// Schema the code in this crate expects. Bumped by adding a step below.
pub(crate) const SCHEMA_VERSION: i64 = 3;

/// Bring the database up to [`SCHEMA_VERSION`].
///
/// Stepped rather than all-or-nothing: a vault written by an earlier build
/// has entries in it, so version 2 must *add* the task tables beside them
/// rather than recreate the file, and version 3 the calendar tables beside
/// both. Each step is idempotent and runs in its own transaction, and
/// `user_version` is only advanced once they all land.
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

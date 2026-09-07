//! SQLite storage backend.
//!
//! This is the default backend: one database file for records, a directory
//! of chunk-encrypted files for media.
//!
//! ```text
//!   store/
//!     everyday.db      journals + entries, projects + tasks + time blocks
//!     media/           attachment payloads (see everyday_core::blobstore)
//! ```
//!
//! This is also the backend that carries the *task* domain
//! ([`everyday_core::store::tasks`]), which is why the todo app is offered
//! on a SQLite vault and not on a Markdown one.
//!
//! # What is encrypted, and what is not
//!
//! Entry and journal payloads are sealed with the vault's cipher before they
//! reach SQLite, so the database file contains no readable journal text —
//! not in a page, not in the WAL, not in a freelist page left behind by a
//! deleted row.
//!
//! A small amount of structural metadata is stored in the clear, because it
//! is what the indexes are built from:
//!
//! | Column | Leaks |
//! |---|---|
//! | `journal_id` | how many journals exist and how entries divide between them |
//! | `local_date` | which days were written on |
//! | `created_us`, `updated_us` | when entries were written and last edited |
//! | `starred`, `pinned` | which entries are flagged |
//! | `project_id`, `parent_id` | the *shape* of the task tree, not its contents |
//! | task `status`, `priority`, `due_date` | how much work is outstanding and roughly when |
//! | block `start_us`, `end_us`, `local_date`, `kind` | that time was booked, never to what |
//! | event `calendar_id`, `local_date`, `end_date`, `start_us` | how many calendars, and which days have something on them |
//!
//! Titles, bodies, tags, locations, attachments and file names are all
//! sealed. Someone with the database file learns *that* you journalled on 14
//! July 2024 and never what you wrote. This is the same trade Day One makes,
//! and it is what allows date-range queries and pagination to run as index
//! scans rather than decrypting the entire vault on every keystroke.
//!
//! The task tables make the same trade for the same reason -- a board filters
//! by status and a calendar by day, and both would otherwise decrypt every
//! row on every draw. Task titles, descriptions and *tags* stay sealed, so
//! the file says that four things are blocked and never what they are.
//!
//! The calendar tables go further than the others in one respect: a feed's
//! *address* is sealed along with everything else. A subscription URL is a
//! bearer credential -- anyone holding one can read that calendar for as long
//! as it is not revoked -- so it never sits in a clear column, and neither
//! does the name of the calendar it points at.
//!
//! If that trade is not acceptable, the abstraction is the answer: a backend
//! that seals the index columns too — at the cost of full scans — plugs in
//! without the rest of the app noticing.

use everyday_core::blobstore::FileBlobStore;
use everyday_core::calendar::{Calendar, Event};
use everyday_core::error::{Error, Result};
use everyday_core::id::{BlobId, EntryId, JournalId};
use everyday_core::id::{BlockId, CalendarId, EventId, ProjectId, TaskId};
use everyday_core::model::{Entry, EntrySummary, Journal};
use everyday_core::store::calendars::{CalendarStore, EventQuery, calendar_aad, event_aad};
use everyday_core::store::tasks::{
    BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore, block_aad, project_aad,
    task_aad,
};
use everyday_core::store::{
    Capabilities, EntryQuery, JournalStore, SortOrder, StoreContext, StoreFactory, StoreStats,
    entry_aad, journal_aad,
};
use everyday_core::task::{
    BlockKind, Project, ProjectTaskCount, Task, TaskStats, TaskStatus, TimeBlock,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use std::sync::{Arc, Mutex};

pub const BACKEND_ID: &str = "sqlite";
const DB_FILENAME: &str = "everyday.db";
const MEDIA_DIRNAME: &str = "media";
const SCHEMA_VERSION: i64 = 3;

/// Registers this backend with a [`everyday_core::BackendRegistry`].
pub struct SqliteFactory;

impl StoreFactory for SqliteFactory {
    fn id(&self) -> &'static str {
        BACKEND_ID
    }

    fn describe(&self) -> &'static str {
        "SQLite database \u{2014} fastest, best for large journals (recommended)"
    }

    fn open(&self, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
        Ok(Box::new(SqliteStore::open(ctx)?))
    }
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
    blobs: FileBlobStore,
    cipher: Arc<dyn everyday_core::crypto::Cipher>,
}

impl SqliteStore {
    pub fn open(ctx: StoreContext) -> Result<Self> {
        std::fs::create_dir_all(&ctx.root).map_err(|e| Error::io(&ctx.root, e))?;
        let db_path = ctx.root.join(DB_FILENAME);
        let conn = Connection::open(&db_path).map_err(Error::backend)?;

        // WAL keeps a slow fsync from blocking reads, which is what keeps
        // typing smooth while an autosave is in flight. `NORMAL` sync is the
        // documented safe pairing with WAL: durable across process crashes,
        // and a power loss can lose only the last transaction.
        conn.pragma_update(None, "journal_mode", "WAL").map_err(Error::backend)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(Error::backend)?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(Error::backend)?;
        // Overwrite deleted pages rather than leaving stale ciphertext (and
        // cleartext dates) in the free list.
        conn.pragma_update(None, "secure_delete", "ON").map_err(Error::backend)?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(Error::backend)?;

        migrate(&conn)?;

        Ok(Self {
            blobs: FileBlobStore::open(ctx.root.join(MEDIA_DIRNAME), ctx.cipher.clone())?,
            cipher: ctx.cipher,
            conn: Mutex::new(conn),
        })
    }

    fn seal_entry(&self, e: &Entry) -> Result<(Vec<u8>, Vec<u8>)> {
        let aad = entry_aad(e.id);
        let data = self.cipher.seal(&aad, &serde_json::to_vec(e)?)?;
        let summary = self.cipher.seal(&aad, &serde_json::to_vec(&e.summarize())?)?;
        Ok((data, summary))
    }

    fn open_entry(&self, id: EntryId, sealed: &[u8]) -> Result<Entry> {
        let plain = self.cipher.open(&entry_aad(id), sealed)?;
        Ok(serde_json::from_slice(&plain)?)
    }

    fn open_summary(&self, id: EntryId, sealed: &[u8]) -> Result<EntrySummary> {
        let plain = self.cipher.open(&entry_aad(id), sealed)?;
        Ok(serde_json::from_slice(&plain)?)
    }
}

/// Bring the database up to [`SCHEMA_VERSION`].
///
/// Stepped rather than all-or-nothing: a vault written by an earlier build
/// has entries in it, so version 2 must *add* the task tables beside them
/// rather than recreate the file, and version 3 the calendar tables beside
/// both. Each step is idempotent and runs in its own transaction, and
/// `user_version` is only advanced once they all land.
fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 =
        conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(Error::backend)?;
    if version >= SCHEMA_VERSION {
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

/// Microseconds since the Unix epoch. Stored as an integer rather than an
/// RFC 3339 string because string timestamps only sort correctly if the
/// fractional-second width never varies, which is a fragile thing to rely on.
fn to_us(ts: jiff::Timestamp) -> i64 {
    ts.as_microsecond()
}

impl JournalStore for SqliteStore {
    fn backend(&self) -> &'static str {
        BACKEND_ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            blobs: true,
            transactional: true,
            human_readable: false,
            max_blob_bytes: None,
            tasks: true,
            calendars: true,
        }
    }

    fn tasks(&self) -> Option<&dyn TaskStore> {
        Some(self)
    }

    fn calendars(&self) -> Option<&dyn CalendarStore> {
        Some(self)
    }

    // ---- journals -------------------------------------------------------

    fn list_journals(&self) -> Result<Vec<Journal>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM journals ORDER BY sort_order, id")
            .map_err(Error::backend)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map_err(Error::backend)?;

        let mut out = Vec::new();
        for row in rows {
            let (id, sealed) = row.map_err(Error::backend)?;
            let id = JournalId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?;
            let plain = self.cipher.open(&journal_aad(id), &sealed)?;
            out.push(serde_json::from_slice(&plain)?);
        }
        Ok(out)
    }

    fn get_journal(&self, id: JournalId) -> Result<Journal> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM journals WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        let sealed = sealed.ok_or_else(|| Error::not_found("journal", id))?;
        let plain = self.cipher.open(&journal_aad(id), &sealed)?;
        Ok(serde_json::from_slice(&plain)?)
    }

    fn put_journal(&self, j: &Journal) -> Result<()> {
        let sealed = self.cipher.seal(&journal_aad(j.id), &serde_json::to_vec(j)?)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO journals (id, sort_order, updated_us, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET sort_order = ?2, updated_us = ?3, data = ?4",
            params![j.id.to_string(), j.sort_order, to_us(j.updated_at), sealed],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_journal(&self, id: JournalId) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        tx.execute("DELETE FROM entries WHERE journal_id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.execute("DELETE FROM journals WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    // ---- entries --------------------------------------------------------

    fn list_entries(&self, query: &EntryQuery) -> Result<Vec<EntrySummary>> {
        // Push down what the indexes can answer. Two things cannot be pushed
        // down, because they live inside the sealed payload:
        //
        //   * tag filtering  - tags are encrypted
        //   * title ordering - titles are encrypted
        //
        // When either is in play we fetch the filtered set and finish in
        // memory via `EntryQuery::apply`, which is the same code path the
        // other backends use, so results stay identical across backends.
        let needs_memory_pass = !query.tags.is_empty() || query.sort == SortOrder::TitleAsc;

        let mut sql = String::from("SELECT id, summary FROM entries WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(j) = query.journal_id {
            args.push(Box::new(j.to_string()));
            sql.push_str(&format!(" AND journal_id = ?{}", args.len()));
        }
        if let Some(from) = query.from {
            args.push(Box::new(from.to_string()));
            sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Box::new(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(starred) = query.starred {
            args.push(Box::new(starred));
            sql.push_str(&format!(" AND starred = ?{}", args.len()));
        }

        if !needs_memory_pass {
            sql.push_str(" ORDER BY pinned DESC, ");
            sql.push_str(match query.sort {
                SortOrder::DateDesc => "local_date DESC, created_us DESC",
                SortOrder::DateAsc => "local_date ASC, created_us ASC",
                SortOrder::UpdatedDesc => "updated_us DESC",
                SortOrder::CreatedDesc => "created_us DESC",
                SortOrder::TitleAsc => unreachable!("handled by the in-memory pass"),
            });
            // SQLite needs an explicit LIMIT before it will honour OFFSET.
            sql.push_str(&format!(
                " LIMIT {} OFFSET {}",
                query.limit.map_or(-1i64, i64::from),
                query.offset
            ));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(Error::backend)?;

        let mut out = Vec::new();
        for row in rows {
            let (id, sealed) = row.map_err(Error::backend)?;
            let id = EntryId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?;
            out.push(self.open_summary(id, &sealed)?);
        }

        Ok(if needs_memory_pass { query.apply(out) } else { out })
    }

    fn get_entry(&self, id: EntryId) -> Result<Entry> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM entries WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        let sealed = sealed.ok_or_else(|| Error::not_found("entry", id))?;
        self.open_entry(id, &sealed)
    }

    fn put_entry(&self, e: &Entry) -> Result<()> {
        let (data, summary) = self.seal_entry(e)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO entries
                (id, journal_id, local_date, created_us, updated_us, starred, pinned, data, summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                journal_id = ?2, local_date = ?3, created_us = ?4, updated_us = ?5,
                starred = ?6, pinned = ?7, data = ?8, summary = ?9",
            params![
                e.id.to_string(),
                e.journal_id.to_string(),
                e.local_date.to_string(),
                to_us(e.created_at),
                to_us(e.updated_at),
                e.starred,
                e.pinned,
                data,
                summary,
            ],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_entry(&self, id: EntryId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM entries WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }

    fn all_entries(&self) -> Result<Vec<Entry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, data FROM entries ORDER BY local_date DESC, created_us DESC")
                .map_err(Error::backend)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map_err(Error::backend)?;

        let mut out = Vec::new();
        for row in rows {
            let (id, sealed) = row.map_err(Error::backend)?;
            let id = EntryId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?;
            out.push(self.open_entry(id, &sealed)?);
        }
        Ok(out)
    }

    // ---- blobs ----------------------------------------------------------

    fn put_blob(&self, bytes: &[u8]) -> Result<BlobId> {
        self.blobs.put(bytes)
    }

    fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
        self.blobs.get(id)
    }

    fn blob_len(&self, id: BlobId) -> Result<u64> {
        self.blobs.len_of(id)
    }

    fn get_blob_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.blobs.get_range(id, offset, len)
    }

    fn has_blob(&self, id: BlobId) -> Result<bool> {
        Ok(self.blobs.has(id))
    }

    fn delete_blob(&self, id: BlobId) -> Result<()> {
        self.blobs.delete(id)
    }

    fn list_blobs(&self) -> Result<Vec<BlobId>> {
        self.blobs.list()
    }

    // ---- housekeeping ---------------------------------------------------

    fn stats(&self) -> Result<StoreStats> {
        let conn = self.conn.lock().unwrap();
        let journals: i64 =
            conn.query_row("SELECT COUNT(*) FROM journals", [], |r| r.get(0)).map_err(Error::backend)?;
        let entries: i64 =
            conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0)).map_err(Error::backend)?;
        drop(conn);
        let (blobs, blob_bytes) = self.blobs.stats()?;
        Ok(StoreStats {
            journals: journals as u64,
            entries: entries as u64,
            blobs,
            blob_bytes,
        })
    }

    fn flush(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").map_err(Error::backend)?;
        Ok(())
    }
}

// ── The task domain ──────────────────────────────────────────────────────

/// `YYYY-MM-DD`, which sorts lexicographically as it sorts chronologically,
/// so a text column is a usable index for date ranges.
fn date_str(d: Option<jiff::civil::Date>) -> Option<String> {
    d.map(|d| d.to_string())
}

fn id_str<T: std::fmt::Display>(id: Option<T>) -> Option<String> {
    id.map(|i| i.to_string())
}

impl SqliteStore {
    fn seal<T: serde::Serialize>(&self, aad: &[u8], value: &T) -> Result<Vec<u8>> {
        self.cipher.seal(aad, &serde_json::to_vec(value)?)
    }

    fn unseal<T: serde::de::DeserializeOwned>(&self, aad: &[u8], sealed: &[u8]) -> Result<T> {
        Ok(serde_json::from_slice(&self.cipher.open(aad, sealed)?)?)
    }

    /// Decrypt a `(id, data)` result set into whole records.
    fn collect<T, I>(
        &self,
        rows: Vec<(String, Vec<u8>)>,
        aad: impl Fn(I) -> Vec<u8>,
    ) -> Result<Vec<T>>
    where
        T: serde::de::DeserializeOwned,
        I: std::str::FromStr + Copy,
        I::Err: std::fmt::Display,
    {
        rows.into_iter()
            .map(|(id, sealed)| {
                let id: I = id.parse().map_err(|e: I::Err| Error::Invalid(e.to_string()))?;
                self.unseal(&aad(id), &sealed)
            })
            .collect()
    }

    /// Every task in the subtree rooted at `root`, `root` itself included.
    ///
    /// Walked breadth-first in Rust rather than as a recursive CTE so that
    /// the cascade -- which has to reach the `time_blocks` table too -- is
    /// one readable rule in one place. Subtrees are tens of rows, not
    /// millions, so the extra round trips do not signify.
    fn subtree(tx: &Transaction<'_>, root: TaskId) -> Result<Vec<String>> {
        let mut out = vec![root.to_string()];
        let mut frontier = vec![root.to_string()];
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for parent in &frontier {
                let mut stmt = tx
                    .prepare_cached("SELECT id FROM tasks WHERE parent_id = ?1")
                    .map_err(Error::backend)?;
                let kids = stmt
                    .query_map(params![parent], |r| r.get::<_, String>(0))
                    .map_err(Error::backend)?;
                for kid in kids {
                    let kid = kid.map_err(Error::backend)?;
                    // A cycle would be corrupt data, but a corrupt vault
                    // should not hang the app -- so believe the set, not the
                    // shape.
                    if !out.contains(&kid) {
                        out.push(kid.clone());
                        next.push(kid);
                    }
                }
            }
            frontier = next;
        }
        Ok(out)
    }

    /// Delete these tasks and every time block booked against them.
    fn purge_tasks(tx: &Transaction<'_>, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let holes = std::iter::repeat_n("?", ids.len()).collect::<Vec<_>>().join(",");
        let args = params_from_iter(ids.iter());
        tx.execute(&format!("DELETE FROM time_blocks WHERE task_id IN ({holes})"), args)
            .map_err(Error::backend)?;
        tx.execute(
            &format!("DELETE FROM tasks WHERE id IN ({holes})"),
            params_from_iter(ids.iter()),
        )
        .map_err(Error::backend)?;
        Ok(())
    }
}

impl TaskStore for SqliteStore {
    // ---- projects -------------------------------------------------------

    fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM projects ORDER BY sort_order, created_us")
            .map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, project_aad)
    }

    fn get_project(&self, id: ProjectId) -> Result<Project> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM projects WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("project", id))?;
        self.unseal(&project_aad(id), &sealed)
    }

    fn put_project(&self, p: &Project) -> Result<()> {
        let data = self.seal(&project_aad(p.id), p)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO projects
                (id, status, priority, due_date, sort_order, created_us, updated_us,
                 completed_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                status = ?2, priority = ?3, due_date = ?4, sort_order = ?5,
                created_us = ?6, updated_us = ?7, completed_us = ?8, data = ?9",
            params![
                p.id.to_string(),
                p.status.as_str(),
                p.priority.rank(),
                date_str(p.due_date),
                p.sort_order,
                to_us(p.created_at),
                to_us(p.updated_at),
                p.completed_at.map(to_us),
                data,
            ],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_project(&self, id: ProjectId) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;

        // Its tasks, plus anything nested under them -- a subtask filed into
        // a different project is still doomed with its parent.
        let roots: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT id FROM tasks WHERE project_id = ?1").map_err(Error::backend)?;
            stmt.query_map(params![id.to_string()], |r| r.get::<_, String>(0))
                .map_err(Error::backend)?
                .collect::<rusqlite::Result<_>>()
                .map_err(Error::backend)?
        };
        let mut doomed: Vec<String> = Vec::new();
        for root in roots {
            let root = TaskId::parse(&root).map_err(|e| Error::Invalid(e.to_string()))?;
            for descendant in Self::subtree(&tx, root)? {
                if !doomed.contains(&descendant) {
                    doomed.push(descendant);
                }
            }
        }
        Self::purge_tasks(&tx, &doomed)?;

        // Time booked against the project itself, not against its tasks.
        tx.execute("DELETE FROM time_blocks WHERE project_id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.execute("DELETE FROM projects WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    // ---- tasks ----------------------------------------------------------

    fn list_tasks(&self, query: &TaskQuery) -> Result<Vec<Task>> {
        // The same split the entry list makes: push down what the indexes
        // can answer, and finish in memory when a filter or an ordering
        // depends on something that only exists once decrypted -- here the
        // tags, the free-text match and the title ordering.
        let needs_memory_pass = !query.tags.is_empty()
            || !query.text.trim().is_empty()
            || query.sort == TaskSort::TitleAsc;

        let mut sql = String::from("SELECT id, data FROM tasks WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        match query.project {
            ProjectScope::Any => {}
            ProjectScope::Inbox => sql.push_str(" AND project_id IS NULL"),
            ProjectScope::Project { id } => {
                args.push(Box::new(id.to_string()));
                sql.push_str(&format!(" AND project_id = ?{}", args.len()));
            }
        }
        match query.parent {
            ParentScope::Any => {}
            ParentScope::TopLevel => sql.push_str(" AND parent_id IS NULL"),
            ParentScope::Of { id } => {
                args.push(Box::new(id.to_string()));
                sql.push_str(&format!(" AND parent_id = ?{}", args.len()));
            }
        }
        if !query.statuses.is_empty() {
            let mut holes = Vec::with_capacity(query.statuses.len());
            for status in &query.statuses {
                args.push(Box::new(status.as_str()));
                holes.push(format!("?{}", args.len()));
            }
            sql.push_str(&format!(" AND status IN ({})", holes.join(",")));
        }
        if let Some(min) = query.priority_at_least {
            args.push(Box::new(min.rank()));
            sql.push_str(&format!(" AND priority >= ?{}", args.len()));
        }
        if let Some(want) = query.has_due {
            sql.push_str(if want { " AND due_date IS NOT NULL" } else { " AND due_date IS NULL" });
        }
        // NULL compares as neither >= nor <=, so an undated task falls out
        // of a date window here for the same reason it does in
        // `TaskQuery::matches`. That agreement is what the conformance suite
        // pins down.
        if let Some(from) = query.due_from {
            args.push(Box::new(from.to_string()));
            sql.push_str(&format!(" AND due_date >= ?{}", args.len()));
        }
        if let Some(to) = query.due_to {
            args.push(Box::new(to.to_string()));
            sql.push_str(&format!(" AND due_date <= ?{}", args.len()));
        }

        if !needs_memory_pass {
            sql.push_str(" ORDER BY ");
            sql.push_str(match query.sort {
                TaskSort::Manual => "sort_order ASC, created_us ASC",
                // "Undated last" has to be said out loud: SQLite sorts NULL
                // first on an ASC column, which would put the whole backlog
                // above the things actually due.
                TaskSort::DueAsc => "due_date IS NULL, due_date ASC, sort_order ASC",
                TaskSort::PriorityDesc => "priority DESC, sort_order ASC",
                TaskSort::CreatedDesc => "created_us DESC",
                TaskSort::UpdatedDesc => "updated_us DESC",
                TaskSort::CompletedDesc => "completed_us IS NULL, completed_us DESC",
                TaskSort::TitleAsc => unreachable!("handled by the in-memory pass"),
            });
            // SQLite needs an explicit LIMIT before it will honour OFFSET.
            sql.push_str(&format!(
                " LIMIT {} OFFSET {}",
                query.limit.map_or(-1i64, i64::from),
                query.offset
            ));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);

        let tasks: Vec<Task> = self.collect(rows, task_aad)?;
        Ok(if needs_memory_pass { query.apply(tasks) } else { tasks })
    }

    fn get_task(&self, id: TaskId) -> Result<Task> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM tasks WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("task", id))?;
        self.unseal(&task_aad(id), &sealed)
    }

    fn put_task(&self, t: &Task) -> Result<()> {
        self.put_tasks(std::slice::from_ref(t))
    }

    fn put_tasks(&self, tasks: &[Task]) -> Result<()> {
        if tasks.is_empty() {
            return Ok(());
        }
        // Sealed before the lock is taken: encryption is the expensive part
        // and there is no reason to hold the connection through it.
        let sealed: Vec<(&Task, Vec<u8>)> =
            tasks.iter().map(|t| Ok((t, self.seal(&task_aad(t.id), t)?))).collect::<Result<_>>()?;

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        for (t, data) in sealed {
            tx.execute(
                "INSERT INTO tasks
                    (id, project_id, parent_id, status, priority, start_date, due_date,
                     sort_order, created_us, updated_us, completed_us, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET
                    project_id = ?2, parent_id = ?3, status = ?4, priority = ?5,
                    start_date = ?6, due_date = ?7, sort_order = ?8, created_us = ?9,
                    updated_us = ?10, completed_us = ?11, data = ?12",
                params![
                    t.id.to_string(),
                    id_str(t.project_id),
                    id_str(t.parent_id),
                    t.status.as_str(),
                    t.priority.rank(),
                    date_str(t.start_date),
                    date_str(t.due_date),
                    t.sort_order,
                    to_us(t.created_at),
                    to_us(t.updated_at),
                    t.completed_at.map(to_us),
                    data,
                ],
            )
            .map_err(Error::backend)?;
        }
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    fn delete_task(&self, id: TaskId) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        let doomed = Self::subtree(&tx, id)?;
        Self::purge_tasks(&tx, &doomed)?;
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    // ---- time blocks ----------------------------------------------------

    fn list_blocks(&self, query: &BlockQuery) -> Result<Vec<TimeBlock>> {
        let mut sql = String::from("SELECT id, data FROM time_blocks WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(from) = query.from {
            args.push(Box::new(from.to_string()));
            sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Box::new(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(task) = query.task_id {
            args.push(Box::new(task.to_string()));
            sql.push_str(&format!(" AND task_id = ?{}", args.len()));
        }
        if let Some(project) = query.project_id {
            args.push(Box::new(project.to_string()));
            sql.push_str(&format!(" AND project_id = ?{}", args.len()));
        }
        if let Some(kind) = query.kind {
            args.push(Box::new(kind.as_str()));
            sql.push_str(&format!(" AND kind = ?{}", args.len()));
        }
        sql.push_str(" ORDER BY start_us ASC, end_us ASC");
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, block_aad)
    }

    fn get_block(&self, id: BlockId) -> Result<TimeBlock> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM time_blocks WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("block", id))?;
        self.unseal(&block_aad(id), &sealed)
    }

    fn put_block(&self, b: &TimeBlock) -> Result<()> {
        let data = self.seal(&block_aad(b.id), b)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO time_blocks
                (id, task_id, project_id, local_date, start_us, end_us, kind, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                task_id = ?2, project_id = ?3, local_date = ?4, start_us = ?5,
                end_us = ?6, kind = ?7, data = ?8",
            params![
                b.id.to_string(),
                id_str(b.subject.task_id()),
                id_str(b.subject.project_id()),
                b.local_date.to_string(),
                to_us(b.start),
                to_us(b.end),
                b.kind.as_str(),
                data,
            ],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_block(&self, id: BlockId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM time_blocks WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }

    // ---- housekeeping ---------------------------------------------------

    fn task_stats(&self, today: jiff::civil::Date) -> Result<TaskStats> {
        // Every count here reads a clear column, so the whole panel costs
        // one pass over the indexes and decrypts nothing.
        let conn = self.conn.lock().unwrap();
        let count = |sql: &str| -> Result<u64> {
            let n: i64 = conn.query_row(sql, [], |r| r.get(0)).map_err(Error::backend)?;
            Ok(n as u64)
        };
        let minutes = |kind: BlockKind| -> Result<u64> {
            // Truncated to whole minutes per block before summing, so the
            // total agrees with the per-block figures the UI shows.
            let secs: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(MAX(end_us - start_us, 0)), 0) / 1000000
                     FROM time_blocks WHERE kind = ?1",
                    params![kind.as_str()],
                    |r| r.get(0),
                )
                .map_err(Error::backend)?;
            Ok((secs / 60) as u64)
        };

        let open: Vec<&str> =
            TaskStatus::ALL.iter().filter(|s| s.is_open()).map(|s| s.as_str()).collect();
        let open_list = open.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");

        // One grouped scan of `tasks_by_project`, which is exactly the index
        // this is shaped like. `project_id` and `status` are both clear
        // columns, so the sidebar's counts cost no decryption at all.
        let mut open_by_project = Vec::new();
        {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT project_id, COUNT(*) FROM tasks
                     WHERE status IN ({open_list}) GROUP BY project_id"
                ))
                .map_err(Error::backend)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)))
                .map_err(Error::backend)?;
            for row in rows {
                let (id, n) = row.map_err(Error::backend)?;
                let project_id = match id {
                    Some(id) => {
                        Some(ProjectId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?)
                    }
                    None => None,
                };
                open_by_project.push(ProjectTaskCount { project_id, open: n as u64 });
            }
        }

        // Both bounds compare against a clear `due_date` column, and NULL
        // compares as neither -- so an undated task is correctly outside
        // both, exactly as it is outside `TaskQuery`'s date windows.
        let today = today.to_string();
        let due_today: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM tasks
                     WHERE status IN ({open_list}) AND due_date <= ?1"
                ),
                params![today],
                |r| r.get(0),
            )
            .map_err(Error::backend)?;
        let overdue: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM tasks
                     WHERE status IN ({open_list}) AND due_date < ?1"
                ),
                params![today],
                |r| r.get(0),
            )
            .map_err(Error::backend)?;

        Ok(TaskStats {
            open_by_project,
            due_today: due_today as u64,
            overdue: overdue as u64,
            projects: count("SELECT COUNT(*) FROM projects")?,
            active_projects: count(
                "SELECT COUNT(*) FROM projects WHERE status IN ('active', 'paused')",
            )?,
            tasks: count("SELECT COUNT(*) FROM tasks")?,
            open_tasks: count(&format!(
                "SELECT COUNT(*) FROM tasks WHERE status IN ({open_list})"
            ))?,
            done_tasks: count("SELECT COUNT(*) FROM tasks WHERE status = 'done'")?,
            blocks: count("SELECT COUNT(*) FROM time_blocks")?,
            planned_minutes: minutes(BlockKind::Planned)?,
            logged_minutes: minutes(BlockKind::Actual)?,
        })
    }
}

// ── The calendar domain ──────────────────────────────────────────────────

impl CalendarStore for SqliteStore {
    // ---- subscriptions --------------------------------------------------

    fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM calendars ORDER BY created_us")
            .map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, calendar_aad)
    }

    fn get_calendar(&self, id: CalendarId) -> Result<Calendar> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM calendars WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("calendar", id))?;
        self.unseal(&calendar_aad(id), &sealed)
    }

    fn put_calendar(&self, c: &Calendar) -> Result<()> {
        // Note what is *not* in the clear columns: the name, and above all
        // the URL. A feed address is a bearer credential.
        let data = self.seal(&calendar_aad(c.id), c)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO calendars (id, visible, created_us, updated_us, synced_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                visible = ?2, created_us = ?3, updated_us = ?4, synced_us = ?5, data = ?6",
            params![
                c.id.to_string(),
                c.visible,
                to_us(c.created_at),
                to_us(c.updated_at),
                c.last_synced_at.map(to_us),
                data,
            ],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_calendar(&self, id: CalendarId) -> Result<()> {
        // The events go with it by foreign key -- but only if the pragma is
        // on, which `open` sets and which a future refactor could quietly
        // turn off. Deleting them explicitly costs one indexed statement and
        // does not depend on a connection setting staying put.
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        tx.execute("DELETE FROM events WHERE calendar_id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.execute("DELETE FROM calendars WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    // ---- events ---------------------------------------------------------

    fn list_events(&self, query: &EventQuery) -> Result<Vec<Event>> {
        // Only the text filter needs the payload -- titles and locations are
        // sealed -- so it is the one thing that cannot be pushed into SQL.
        // Everything else is a clear column, and the window is an overlap
        // test on the two date columns rather than a bound on the start.
        let mut sql = String::from("SELECT id, data FROM events WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(from) = query.from {
            args.push(Box::new(from.to_string()));
            sql.push_str(&format!(" AND end_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Box::new(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(cal) = query.calendar_id {
            args.push(Box::new(cal.to_string()));
            sql.push_str(&format!(" AND calendar_id = ?{}", args.len()));
        }
        if query.visible_only {
            sql.push_str(" AND calendar_id IN (SELECT id FROM calendars WHERE visible = 1)");
        }
        // All-day first within a day, then chronological: what every
        // calendar draws, and therefore where the eye looks for them.
        sql.push_str(" ORDER BY all_day DESC, start_us ASC, end_us ASC");
        // A text filter cuts rows after the fact, so the limit cannot be
        // pushed down with it -- it would cap the wrong set.
        let in_memory_pass = !query.text.trim().is_empty();
        if let Some(limit) = query.limit
            && !in_memory_pass
        {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);

        let out: Vec<Event> = self.collect(rows, event_aad)?;
        Ok(if in_memory_pass { query.apply(out) } else { out })
    }

    fn get_event(&self, id: EventId) -> Result<Event> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM events WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("event", id))?;
        self.unseal(&event_aad(id), &sealed)
    }

    fn replace_events(&self, calendar: CalendarId, events: &[Event]) -> Result<()> {
        // Sealing happens before the lock is taken: a feed can be thousands
        // of occurrences, and holding the connection across that many AEAD
        // seals would stall every other query for the duration of a sync
        // that is meant to be invisible.
        let sealed: Vec<(String, Vec<u8>)> = events
            .iter()
            .map(|e| Ok((e.id.to_string(), self.seal(&event_aad(e.id), e)?)))
            .collect::<Result<_>>()?;

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        tx.execute("DELETE FROM events WHERE calendar_id = ?1", params![calendar.to_string()])
            .map_err(Error::backend)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO events
                        (id, calendar_id, local_date, end_date, start_us, end_us, all_day, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(Error::backend)?;
            for (event, (id, data)) in events.iter().zip(&sealed) {
                // An event claiming to belong to another calendar would
                // survive this sync and be deleted by that calendar's next
                // one. File it where it was asked to go.
                stmt.execute(params![
                    id,
                    calendar.to_string(),
                    event.local_date.to_string(),
                    event.end_date.to_string(),
                    to_us(event.start),
                    to_us(event.end),
                    event.all_day,
                    data,
                ])
                .map_err(Error::backend)?;
            }
        }
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    fn count_events(&self, calendar: CalendarId) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE calendar_id = ?1",
                params![calendar.to_string()],
                |r| r.get(0),
            )
            .map_err(Error::backend)?;
        Ok(n as u64)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use everyday_core::crypto::{AeadCipher, Cipher, NullCipher, SecretKey};
    use everyday_core::store::conformance;
    use everyday_core::task::{Project, Task, TimeBlock};
    use everyday_core::{RichDoc, model::Journal};
    use std::path::Path;

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
        let sql_path = EntryQuery {
            sort: SortOrder::DateAsc,
            offset: 5,
            limit: Some(4),
            ..Default::default()
        };
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
        let cal =
            everyday_core::calendar::Calendar::subscribed("Work", "https://example.com/work.ics");
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
        let window =
            EventQuery::between(jiff::civil::date(2026, 6, 3), jiff::civil::date(2026, 6, 7));
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
            everyday_core::calendar::Calendar::subscribed("Shown", "https://example.com/a.ics");
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
}

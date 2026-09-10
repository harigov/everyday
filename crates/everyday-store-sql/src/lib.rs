//! The SQL storage backend, once, for every SQL database Every Day speaks.
//!
//! This is where a vault's records actually live. It is one implementation of
//! all five store traits, and it runs unchanged against SQLite on a laptop
//! and against Postgres — including a Supabase project — over the network.
//! The crates beside it (`everyday-store-sqlite`, `everyday-store-postgres`)
//! are drivers: each supplies a connection, a [`Dialect`] and a handful of
//! answers this crate cannot give for it, and gets the whole backend back.
//!
//! ```text
//!   everyday-store-sql        the schema, the queries, the cascades, the
//!                             clear/sealed split -- all of it, once
//!     |
//!     +-- everyday-store-sqlite    rusqlite + pragmas + VACUUM INTO
//!     +-- everyday-store-postgres  postgres + TLS + a version table
//! ```
//!
//! # What is encrypted, and what is not
//!
//! Entry and journal payloads are sealed with the vault's cipher before they
//! reach the database, so the file — or the server — contains no readable
//! journal text. Not in a page, not in the WAL, not in a freelist page left
//! behind by a deleted row, and not on the wire.
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
//! | item `kind_id`, `status`, `rating`, `favourite`, `year`, `finished_on` | how many shelves, how much is on each, and how you scored it |
//! | log `item_id`, `event`, `local_date` | that something was got to the end of on a day, never what |
//! | reading `tracker_id`, `local_date`, `at_us`, `value` | that something was recorded, when, and how much of it -- never what |
//! | tracker `archived`, `sort_order` | how many things are tracked and which have been retired |
//! | goal `role_id`, `status`, `horizon` | how many goals sit under each part of a life, how they are going, and roughly when they are wanted |
//! | role `archived`, `sort_order` | how many parts a life is divided into |
//! | `purposes` (`record_kind`, `record_id`, `purpose_kind`, `purpose_id`) | which records are filed against which goal -- never the name of either |
//! | note `pinned`, `created_us`, `updated_us` | how many notes there are, which are pinned, and when they were touched -- never a title |
//! | routine `created_us`, `updated_us` | how many standing jobs the assistant has |
//! | run `routine_id`, `started_us`, `seen` | that a routine ran at seven and that nobody has read the result |
//!
//! Titles, bodies, tags, locations, attachments and file names are all
//! sealed. Someone with the database learns *that* you journalled on 14 July
//! 2024 and never what you wrote. This is the same trade Day One makes, and
//! it is what allows date-range queries and pagination to run as index scans
//! rather than decrypting the entire vault on every keystroke.
//!
//! The task tables make the same trade for the same reason -- a board filters
//! by status and a calendar by day, and both would otherwise decrypt every
//! row on every draw. Task titles, descriptions and *tags* stay sealed, so
//! the database says that four things are blocked and never what they are.
//!
//! The calendar tables go further than the others in one respect: a feed's
//! *address* is sealed along with everything else. A subscription URL is a
//! bearer credential -- anyone holding one can read that calendar for as long
//! as it is not revoked -- so it never sits in a clear column, and neither
//! does the name of the calendar it points at.
//!
//! ## What this means for a hosted database
//!
//! Sealing happens on this side of the connection, which is the whole reason
//! a remote backend is offerable at all. Supabase holds ciphertext and the
//! index columns above; it never holds a key, and there is no key for it to
//! hold — the vault password never leaves the machine it is typed on. The
//! honest statement of the trade is the table above: a hosted vault leaks its
//! *shape* to whoever runs the server, in exactly the way a local one leaks
//! it to whoever has the file.
//!
//! If that trade is not acceptable, the abstraction is the answer: a backend
//! that seals the index columns too — at the cost of full scans — plugs in
//! without the rest of the app noticing.
//!
//! # Where the code is
//!
//! ```text
//!   lib.rs         the store itself, and the sealing helpers every
//!                  domain shares
//!   conn.rs        the two-method database a driver has to supply
//!   dialect.rs     the five places SQLite and Postgres disagree
//!   schema.rs      the tables, and the migrations that reach them
//!   blobs.rs       attachments, in a table, for a store with no local disk
//!   journals.rs    impl JournalStore -- journals, entries, blobs
//!   tasks.rs       impl TaskStore    -- projects, tasks, time blocks
//!   calendars.rs   impl CalendarStore -- subscriptions and their events
//!   library.rs     impl LibraryStore  -- shelves, items and the log
//!   trackers.rs    impl TrackerStore  -- the readings a journal recorded
//!   agent.rs       impl AgentStore    -- the assistant's threads and memory
//! ```
//!
//! One [`SqlStore`] implements all six traits; the split is by domain, the
//! same one `everyday_core::store` makes between the trait and its five
//! optional siblings.

pub mod blobs;
pub mod conn;
pub mod dialect;
pub mod schema;

mod agent;
mod calendars;
mod journals;
mod library;
mod notes;
mod pool;
mod profile;
mod purpose;
mod routines;
mod tasks;
mod trackers;

use conn::{Connection, Sql, SqlExt, Value};
use dialect::Dialect;
use everyday_core::blobstore::FileBlobStore;
use everyday_core::crypto::Cipher;
use everyday_core::error::{Error, Result};
use everyday_core::id::{EntryId, ReadingId, TaskId};
use everyday_core::model::{Entry, EntrySummary};
use everyday_core::store::trackers::reading_aad;
use everyday_core::store::{Capabilities, StoreContext, entry_aad};
use pool::{Pool, ReadGuard};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// The handful of things this crate cannot answer for a particular database.
///
/// Everything a driver has to say beyond "here is a connection". Kept
/// deliberately small: if a method lands here that is really a *query*, it
/// belongs in a domain module written once instead.
pub trait Driver: schema::VersionStore + Send + Sync {
    /// Written into the vault header, so the right driver is chosen when the
    /// vault is reopened. `"sqlite"`, `"postgres"`.
    fn backend_id(&self) -> &'static str;

    fn dialect(&self) -> Dialect;

    /// Open another connection to the same database, for the read pool.
    ///
    /// `None` -- the default -- means this driver has only the one it was
    /// opened with, and every read shares the write connection. That is the
    /// old behaviour and remains correct; it is merely serial.
    ///
    /// The connection must come back configured exactly as the first one
    /// was. Both databases keep some of that per *session* rather than per
    /// database -- SQLite's `foreign_keys` and `busy_timeout` are pragmas on
    /// a connection, Postgres's `search_path` is a session setting -- so a
    /// reader that skipped them would answer different questions from the
    /// writer, which is the worst kind of bug this pool could have.
    fn connect(&self) -> Result<Option<Box<dyn Connection>>> {
        Ok(None)
    }

    /// Most reader connections to keep. Ignored by a driver whose
    /// [`connect`](Driver::connect) answers `None`.
    ///
    /// Four is chosen against what actually reads at once: a window drawing a
    /// list while its assistant runs a tool, times a couple of clients. A
    /// larger pool would mostly buy idle Postgres sessions.
    fn read_pool_size(&self) -> usize {
        4
    }

    /// Largest attachment this database will take, if it has a limit.
    ///
    /// `None` for a store whose media go to the filesystem. Postgres caps a
    /// single value at 1 GB, so a backend keeping blobs in a column has a
    /// real ceiling and should say so rather than fail at paste time.
    fn max_blob_bytes(&self) -> Option<u64> {
        None
    }

    /// Force pending writes to durable storage. A no-op for a server, which
    /// has already committed by the time it answered.
    fn flush(&self, _conn: &mut dyn Sql) -> Result<()> {
        Ok(())
    }

    /// Check the database's own consistency. See
    /// [`JournalStore::check_integrity`](everyday_core::JournalStore::check_integrity).
    fn check_integrity(&self, _conn: &mut dyn Sql) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Write a consistent copy of the database into `dir`.
    ///
    /// `root` is the directory the store owns, so a driver that also keeps
    /// media on disk can copy that beside it. The default refuses, which is
    /// the right answer for a database this process does not own the files
    /// of: a hosted Postgres is backed up by whoever hosts it, and pretending
    /// otherwise would produce a "backup" that was not one.
    fn snapshot(&self, _conn: &mut dyn Sql, _root: &Path, _dir: &Path) -> Result<()> {
        Err(Error::Unsupported("backing up this storage backend"))
    }
}

/// Where this store keeps attachment payloads.
///
/// Two answers, because the right one depends on whether the database is on
/// this machine. Both hold the *same sealed bytes* — the container format in
/// [`everyday_core::blobstore`] — so the choice is about where, never about
/// what.
pub enum Media {
    /// A directory of sealed files beside the database. What a local vault
    /// wants: the database stays small, fast to open and cheap to back up,
    /// and a range read into a video is a `seek`.
    Files(FileBlobStore),
    /// A `blobs` table. What a *remote* vault needs, because attachments in
    /// a directory on one laptop are not in the vault at all — they are
    /// missing from every other machine that opens it, and gone with the
    /// laptop. See [`blobs`].
    Table,
}

impl Media {
    /// A directory of sealed files at `dir`.
    pub fn files(dir: impl Into<PathBuf>, cipher: Arc<dyn Cipher>) -> Result<Self> {
        Ok(Media::Files(FileBlobStore::open(dir, cipher)?))
    }
}

/// A vault's records in a SQL database.
pub struct SqlStore {
    driver: Arc<dyn Driver>,
    dialect: Dialect,
    pool: Pool,
    media: Media,
    cipher: Arc<dyn Cipher>,
    /// The directory this store owns, kept so `snapshot` can copy the media
    /// tree beside the database it writes.
    root: PathBuf,
}

impl SqlStore {
    /// Migrate `conn` up to the current schema and wrap it as a store.
    pub fn open(
        driver: Arc<dyn Driver>,
        mut conn: Box<dyn Connection>,
        media: Media,
        ctx: &StoreContext,
    ) -> Result<Self> {
        let dialect = driver.dialect();
        // Migrate before the pool exists, on the connection that will become
        // the writer. A reader opened mid-migration would see half a schema.
        schema::migrate(conn.as_mut(), dialect, driver.as_ref())?;
        if matches!(media, Media::Table) {
            blobs::create_table(conn.as_mut(), dialect)?;
        }
        Ok(Self {
            pool: Pool::new(driver.clone(), conn),
            driver,
            dialect,
            media,
            cipher: ctx.cipher.clone(),
            root: ctx.root.clone(),
        })
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// What every driver of this crate can do.
    ///
    /// All seven domains, on both databases. The optional accessors on
    /// `JournalStore` stay optional for the sake of backends that are not
    /// this one, not because a SQL vault might be missing the todo app.
    pub(crate) fn capabilities(&self) -> Capabilities {
        Capabilities {
            blobs: true,
            transactional: true,
            human_readable: false,
            max_blob_bytes: self.driver.max_blob_bytes(),
            tasks: true,
            calendars: true,
            library: true,
            trackers: true,
            goals: true,
            notes: true,
            routines: true,
            agent: true,
        }
    }

    /// The write connection. One at a time, which is the rule the vault's
    /// own write lock has always promised.
    ///
    /// Every statement that changes anything goes through here, including
    /// the read half of a read-modify-write: taking the value on a reader
    /// and writing it back on the writer is exactly the race the conditional
    /// save exists to prevent, and it would not be caught by it.
    pub(crate) fn write(&self) -> MutexGuard<'_, Box<dyn Connection>> {
        self.pool.write()
    }

    /// A connection to read on. Several reads may hold one at once.
    ///
    /// Do not hold it while taking another guard: see [`pool`].
    pub(crate) fn read(&self) -> ReadGuard<'_> {
        self.pool.read()
    }

    /// Borrow a read connection for SQL this crate does not own.
    ///
    /// The escape hatch, and deliberately a narrow one: a *domain* query
    /// belongs in a module here, written once and run on both databases,
    /// rather than at the far end of this. What it is for is a driver's own
    /// tests and anything diagnostic.
    #[doc(hidden)]
    pub fn with_read<R>(&self, f: impl FnOnce(&mut dyn Sql) -> Result<R>) -> Result<R> {
        f(&mut self.read())
    }

    // ---- sealing --------------------------------------------------------

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

    pub(crate) fn seal<T: serde::Serialize>(&self, aad: &[u8], value: &T) -> Result<Vec<u8>> {
        self.cipher.seal(aad, &serde_json::to_vec(value)?)
    }

    pub(crate) fn unseal<T: serde::de::DeserializeOwned>(
        &self,
        aad: &[u8],
        sealed: &[u8],
    ) -> Result<T> {
        Ok(serde_json::from_slice(&self.cipher.open(aad, sealed)?)?)
    }

    /// Decrypt an `(id, data)` result set into whole records.
    pub(crate) fn collect<T, I>(
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

    // ---- cascades -------------------------------------------------------

    /// Every task in the subtree rooted at `root`, `root` itself included.
    ///
    /// Walked breadth-first in Rust rather than as a recursive CTE so that
    /// the cascade -- which has to reach the `time_blocks` table too -- is
    /// one readable rule in one place. Subtrees are tens of rows, not
    /// millions, so the extra round trips do not signify. (They are round
    /// trips to a *server* on the Postgres backend, which raises the cost
    /// without changing the answer: a subtree is still tens of rows, and the
    /// alternative is two dialects' worth of recursive CTE to maintain.)
    pub(crate) fn subtree(tx: &mut dyn Sql, root: TaskId) -> Result<Vec<String>> {
        let mut out = vec![root.to_string()];
        let mut frontier = vec![root.to_string()];
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for parent in &frontier {
                let rows = tx.query("SELECT id FROM tasks WHERE parent_id = ?1", &vals![parent])?;
                for row in rows {
                    let kid = row.text(0)?;
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
    pub(crate) fn purge_tasks(tx: &mut dyn Sql, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let holes = placeholders(1, ids.len());
        let args: Vec<Value> = ids.iter().map(|id| Value::Text(id.clone())).collect();
        // The blocks are gone with the tasks, so their pointer rows go too.
        // Collected before the delete, because afterwards there is nothing
        // left to ask which blocks these were.
        let blocks: Vec<String> = tx
            .query(&format!("SELECT id FROM time_blocks WHERE task_id IN ({holes})"), &args)?
            .into_iter()
            .map(|r| r.text(0))
            .collect::<Result<_>>()?;
        tx.execute(&format!("DELETE FROM time_blocks WHERE task_id IN ({holes})"), &args)?;
        tx.execute(&format!("DELETE FROM tasks WHERE id IN ({holes})"), &args)?;
        crate::purpose::forget_purposes(tx, crate::purpose::RecordKind::Block, &blocks)?;
        crate::purpose::forget_purposes(tx, crate::purpose::RecordKind::Task, ids)?;
        Ok(())
    }

    /// Clear the entry pointer on any reading that names `entry`.
    ///
    /// Both copies of it: the clear column the index is built on, and the
    /// one inside the sealed payload. Updating only the column would leave
    /// the record disagreeing with itself, and the sealed copy is the one
    /// that would be believed after a restore.
    ///
    /// A read-modify-reseal per row, which is affordable precisely because
    /// of what it operates on: the handful of things ticked while writing
    /// one entry, on the rare occasion that entry is deleted.
    ///
    /// # All of it on the writer, in one transaction
    ///
    /// This is the one place in the crate that reads a row, changes it and
    /// writes it back, and it is therefore the one place the read pool can
    /// hurt. Taking the `SELECT` on a reader and the `UPDATE` on the writer
    /// leaves a window in which somebody else's `put_reading` lands between
    /// them -- and the reseal then writes the payload this call decrypted,
    /// silently reverting their write. The clear column would be right and the
    /// sealed copy wrong, which is the worse half: the sealed copy is the one
    /// believed after a restore.
    ///
    /// So the whole sequence takes the write connection, and takes it once.
    /// The transaction is what makes the set of rows consistent with itself:
    /// without it a failure part way through would leave some readings
    /// detached and some not.
    pub(crate) fn detach_readings_from(&self, entry: EntryId) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let rows = tx.records(
            "SELECT id, data FROM readings WHERE entry_id = ?1",
            &vals![entry.to_string()],
        )?;

        for (id, sealed) in rows {
            let id = ReadingId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?;
            let aad = reading_aad(id);
            let mut reading: everyday_core::tracker::Reading = self.unseal(&aad, &sealed)?;
            reading.entry_id = None;
            let data = self.seal(&aad, &reading)?;
            tx.execute(
                "UPDATE readings SET entry_id = NULL, data = ?2 WHERE id = ?1",
                &vals![id.to_string(), data],
            )?;
        }
        tx.commit()
    }
}

/// A mutex, whether or not a previous caller panicked holding it.
///
/// `lock().unwrap()` would turn one panic anywhere in this crate into a
/// permanently unusable store: every later call would panic on the poison
/// flag, and in the desktop shell that means a window that still looks fine
/// while nothing it does can be saved. Poisoning is also the wrong signal
/// here -- the state it warns about cannot arise. A panic can only escape
/// mid-statement or mid-transaction, and a driver's transaction rolls back
/// when it is dropped, so the connection an unwinding thread leaves behind is
/// exactly the one it borrowed.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `?1, ?2, ...` for an `IN` list, starting at `from` (1-based).
///
/// The dialect rewrites these on the way out, so they are always written in
/// SQLite's numbering here. See [`Dialect::bind`].
pub(crate) fn placeholders(from: usize, count: usize) -> String {
    (from..from + count).map(|n| format!("?{n}")).collect::<Vec<_>>().join(",")
}

/// Microseconds since the Unix epoch. Stored as an integer rather than an
/// RFC 3339 string because string timestamps only sort correctly if the
/// fractional-second width never varies, which is a fragile thing to rely on.
pub(crate) fn to_us(ts: jiff::Timestamp) -> i64 {
    ts.as_microsecond()
}

pub(crate) fn from_us(us: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_microsecond(us).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

/// `YYYY-MM-DD`, which sorts lexicographically as it sorts chronologically,
/// so a text column is a usable index for date ranges.
pub(crate) fn date_str(d: Option<jiff::civil::Date>) -> Option<String> {
    d.map(|d| d.to_string())
}

pub(crate) fn id_str<T: std::fmt::Display>(id: Option<T>) -> Option<String> {
    id.map(|i| i.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_in_list_numbers_its_holes_from_where_it_was_told_to() {
        assert_eq!(placeholders(1, 3), "?1,?2,?3");
        assert_eq!(placeholders(4, 2), "?4,?5");
        assert_eq!(placeholders(1, 0), "");
    }
}

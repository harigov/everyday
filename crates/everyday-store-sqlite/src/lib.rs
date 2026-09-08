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
//!
//! # Where the code is
//!
//! ```text
//!   lib.rs         opening a database, and the sealing helpers every
//!                  domain shares
//!   schema.rs      the tables, and the migrations that reach them
//!   journals.rs    impl JournalStore -- journals, entries, blobs
//!   tasks.rs       impl TaskStore    -- projects, tasks, time blocks
//!   calendars.rs   impl CalendarStore -- subscriptions and their events
//! ```
//!
//! One `SqliteStore` implements all three traits; the split is by domain,
//! the same one `everyday_core::store` makes between the trait and its two
//! optional siblings. It replaces a single file that had grown past 1,800
//! lines, in which finding the four places a task's `sort_order` is written
//! meant scrolling past the entry queries and the migration SQL.

mod calendars;
mod journals;
mod schema;
mod tasks;

use everyday_core::blobstore::FileBlobStore;
use everyday_core::error::{Error, Result};
use everyday_core::id::{EntryId, TaskId};
use everyday_core::model::{Entry, EntrySummary};
use everyday_core::store::{JournalStore, StoreContext, StoreFactory, entry_aad};
use rusqlite::{Connection, Transaction, params, params_from_iter};
use std::sync::{Arc, Mutex};

use schema::migrate;

pub const BACKEND_ID: &str = "sqlite";
const DB_FILENAME: &str = "everyday.db";
const MEDIA_DIRNAME: &str = "media";

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
    /// The directory this store owns, kept so `snapshot` can copy the media
    /// tree beside the database it writes.
    root: std::path::PathBuf,
}

impl SqliteStore {
    pub fn open(ctx: StoreContext) -> Result<Self> {
        std::fs::create_dir_all(&ctx.root).map_err(|e| Error::io(&ctx.root, e))?;
        let db_path = ctx.root.join(DB_FILENAME);
        let conn = Connection::open(&db_path).map_err(Error::backend)?;

        // WAL keeps a slow fsync from blocking reads, which is what keeps
        // typing smooth while an autosave is in flight.
        conn.pragma_update(None, "journal_mode", "WAL").map_err(Error::backend)?;
        // `FULL` rather than the usual WAL pairing of `NORMAL`. Under
        // `NORMAL` the WAL is not fsynced at commit, so a power cut can roll
        // back not merely the last transaction but everything written since
        // the last checkpoint -- SQLite guarantees the file stays *intact*,
        // not that a committed write survives. That is an acceptable trade
        // for a cache and a poor one for someone's journal, and it costs
        // nothing here: this store commits on a 700ms autosave timer, not in
        // a loop, so the extra fsync is unmeasurable against the pauses
        // between keystrokes.
        conn.pragma_update(None, "synchronous", "FULL").map_err(Error::backend)?;
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
            root: ctx.root,
        })
    }

    /// The connection, whether or not a previous caller panicked holding it.
    ///
    /// `lock().unwrap()` would turn one panic anywhere in this crate into a
    /// permanently unusable store: every later call would panic on the poison
    /// flag, and in the desktop shell that means a window that still looks
    /// fine while nothing it does can be saved. Poisoning is also the wrong
    /// signal here -- the state it warns about cannot arise. A panic can only
    /// escape mid-statement or mid-transaction, and rusqlite's `Transaction`
    /// rolls back when it is dropped, so the connection an unwinding thread
    /// leaves behind is exactly the one it borrowed.
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// Microseconds since the Unix epoch. Stored as an integer rather than an
/// RFC 3339 string because string timestamps only sort correctly if the
/// fractional-second width never varies, which is a fragile thing to rely on.
fn to_us(ts: jiff::Timestamp) -> i64 {
    ts.as_microsecond()
}

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

#[cfg(test)]
mod tests;

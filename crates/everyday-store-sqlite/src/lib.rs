//! SQLite driver for the SQL storage backend.
//!
//! This is the default backend: one database file for records, a directory
//! of chunk-encrypted files for media.
//!
//! ```text
//!   store/
//!     everyday.db      journals + entries, tasks + time, calendars, library
//!     media/           attachment payloads (see everyday_core::blobstore)
//! ```
//!
//! # What is in here, and what is not
//!
//! Not: the schema, the queries, the cascades, or the decision about which
//! columns stay in the clear. All of that is in
//! [`everyday_store_sql`](everyday_store_sql), written once and shared with
//! every other database Every Day speaks — which is what makes "the same
//! vault, on Postgres" a driver rather than a fork.
//!
//! What is: the four things that are true of SQLite and of nothing else.
//!
//! * **The pragmas.** WAL, `synchronous = FULL`, foreign keys and
//!   `secure_delete`, each argued for at [`SqliteStore::open`].
//! * **`PRAGMA user_version`**, the free integer in the file header where the
//!   schema version lives. A server-backed driver needs a table for this;
//!   SQLite gives it away.
//! * **`PRAGMA quick_check`**, which is a real answer to "is this file
//!   damaged" and has no equivalent worth writing elsewhere.
//! * **`VACUUM INTO`**, which is what makes a backup a *database* rather than
//!   a dump — see [`SqliteDriver::snapshot`].
//!
//! Media goes to the filesystem here rather than into a `blobs` table,
//! because the database is on this machine and a 400 MB video in a column
//! would make every backup of the records copy it too. The remote driver
//! makes the opposite choice for the opposite reason.

use everyday_core::error::{Error, Result};
use everyday_core::store::{JournalStore, StoreContext, StoreFactory};
use everyday_store_sql::conn::{Connection, Row, Sql, Transaction, Value};
use everyday_store_sql::dialect::Dialect;
use everyday_store_sql::schema::VersionStore;
use everyday_store_sql::{Driver, Media, SqlStore};
use rusqlite::types::{ToSqlOutput, ValueRef};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const BACKEND_ID: &str = "sqlite";
pub(crate) const DB_FILENAME: &str = "everyday.db";
pub(crate) const MEDIA_DIRNAME: &str = "media";

/// Registers this backend with a [`everyday_core::BackendRegistry`].
pub struct SqliteFactory;

impl StoreFactory for SqliteFactory {
    fn id(&self) -> &'static str {
        BACKEND_ID
    }

    fn name(&self) -> &'static str {
        "On this computer"
    }

    fn describe(&self) -> &'static str {
        "A SQLite database in the vault folder \u{2014} fastest, works offline (recommended)"
    }

    fn open(&self, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
        Ok(Box::new(SqliteStore::open(ctx)?))
    }
}

/// A vault's records in a local SQLite file.
///
/// A thin alias in practice: everything it does is [`SqlStore`]'s, and this
/// type exists so the crate has a name to open with and tests have a name to
/// construct.
pub struct SqliteStore;

impl SqliteStore {
    pub fn open(ctx: StoreContext) -> Result<SqlStore> {
        std::fs::create_dir_all(&ctx.root).map_err(|e| Error::io(&ctx.root, e))?;
        let db_path = ctx.root.join(DB_FILENAME);
        let conn = connect(&db_path)?;
        let media = Media::files(ctx.root.join(MEDIA_DIRNAME), ctx.cipher.clone())?;
        SqlStore::open(Arc::new(SqliteDriver { db_path }), Box::new(SqliteConn(conn)), media, &ctx)
    }
}

/// One connection, configured the way every connection to this file must be.
///
/// Called for the writer and again for each pooled reader. The pragmas below
/// are per *connection*, not per database, so a reader that skipped them
/// would enforce no foreign keys and give up instantly on a busy file --
/// which is exactly the sort of difference that turns into a bug report
/// about a query that "sometimes" fails.
fn connect(db_path: &Path) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db_path).map_err(Error::backend)?;

    // WAL keeps a slow fsync from blocking reads, which is what keeps
    // typing smooth while an autosave is in flight. It is also what makes
    // the read pool worth having: in WAL mode a reader never blocks the
    // writer and the writer never blocks a reader, so several windows can
    // draw a list while one of them saves.
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

    Ok(conn)
}

/// What SQLite answers that no other database does.
pub struct SqliteDriver {
    /// Kept so the read pool can open more connections to the same file.
    db_path: PathBuf,
}

impl Driver for SqliteDriver {
    fn backend_id(&self) -> &'static str {
        BACKEND_ID
    }

    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    /// Another connection to the same file.
    ///
    /// Read-write rather than `SQLITE_OPEN_READ_ONLY`, which would be the
    /// tidier promise and is not worth what it costs: a read-only connection
    /// cannot create the `-wal` and `-shm` files, so it depends on the writer
    /// having got there first and fails in ways that depend on timing and on
    /// whether a checkpoint has just run. The pool's own guard is what stops
    /// a reader writing -- see `ReadGuard::execute` -- and it refuses the
    /// statement rather than the connection, which is a better error.
    fn connect(&self) -> Result<Option<Box<dyn Connection>>> {
        Ok(Some(Box::new(SqliteConn(connect(&self.db_path)?))))
    }

    /// Checkpoint the WAL, so what has been committed is in the database
    /// file rather than beside it.
    ///
    /// Run through `query` rather than `execute`: a checkpoint answers with a
    /// row saying how many WAL frames it moved, and rusqlite refuses to
    /// `execute` a statement that returns one. The row is of no interest
    /// here; being allowed to ask is.
    fn flush(&self, conn: &mut dyn Sql) -> Result<()> {
        conn.query("PRAGMA wal_checkpoint(TRUNCATE)", &[])?;
        Ok(())
    }

    /// `PRAGMA quick_check`, which is the useful three quarters of
    /// `integrity_check` at a fraction of the cost: it verifies page
    /// structure and record sanity but skips the index-versus-table
    /// cross-check. That is the right trade for something that runs on
    /// unlock -- torn pages are what a bad shutdown produces, and they are
    /// exactly what this catches.
    fn check_integrity(&self, conn: &mut dyn Sql) -> Result<Vec<String>> {
        let rows = conn.query("PRAGMA quick_check", &[])?;
        // A healthy database answers with the single row "ok".
        rows.into_iter().map(|r| r.text(0)).filter(|r| !matches!(r, Ok(s) if s == "ok")).collect()
    }

    /// `VACUUM INTO`, plus a copy of the media directory.
    ///
    /// `VACUUM INTO` runs inside a read transaction, so the file it produces
    /// is a point-in-time snapshot even while the app keeps writing -- and it
    /// is a plain database, not a dump, so the backup directory opens as a
    /// vault without a restore step. Copying `everyday.db` with the
    /// filesystem would do neither: it would race the WAL and land a torn
    /// page.
    ///
    /// Media is copied after the database, which is the safe order. A blob
    /// present in the copy that no entry references is reclaimed by the next
    /// GC; an entry referencing a blob that was missed would be an
    /// attachment lost in the backup.
    fn snapshot(&self, conn: &mut dyn Sql, root: &Path, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let db = dir.join(DB_FILENAME);
        if db.exists() {
            return Err(Error::Invalid(format!(
                "{} already holds a vault; back up into an empty directory",
                dir.display()
            )));
        }

        // Bound as a parameter: a backup path can contain a quote.
        conn.execute("VACUUM INTO ?1", &[Value::Text(db.to_string_lossy().into_owned())])?;

        everyday_core::fsutil::copy_tree(&root.join(MEDIA_DIRNAME), &dir.join(MEDIA_DIRNAME))?;
        everyday_core::fsutil::sync_dir(dir);
        Ok(())
    }
}

impl VersionStore for SqliteDriver {
    /// `PRAGMA user_version`: a 32-bit integer in the file header that
    /// SQLite reserves for exactly this and never touches itself.
    fn schema_version(&self, conn: &mut dyn Sql) -> Result<i64> {
        match conn.query("PRAGMA user_version", &[])?.first() {
            Some(row) => row.i64(0),
            None => Ok(0),
        }
    }

    fn set_schema_version(&self, conn: &mut dyn Sql, version: i64) -> Result<()> {
        // Not bindable: SQLite parses a pragma argument at prepare time, so
        // it cannot be a parameter. Safe because the value is this crate's
        // own integer constant and never anything a caller supplied.
        conn.execute(&format!("PRAGMA user_version = {version}"), &[])?;
        Ok(())
    }
}

// ---- the driver proper: rusqlite behind the two-method trait -------------

struct SqliteConn(rusqlite::Connection);

impl Sql for SqliteConn {
    fn execute(&mut self, sql: &str, args: &[Value]) -> Result<u64> {
        exec(&self.0, sql, args)
    }

    fn query(&mut self, sql: &str, args: &[Value]) -> Result<Vec<Row>> {
        fetch(&self.0, sql, args)
    }
}

impl Connection for SqliteConn {
    fn begin(&mut self) -> Result<Box<dyn Transaction + '_>> {
        let tx = self.0.transaction().map_err(Error::backend)?;
        Ok(Box::new(SqliteTx(tx)))
    }
}

struct SqliteTx<'a>(rusqlite::Transaction<'a>);

impl Sql for SqliteTx<'_> {
    fn execute(&mut self, sql: &str, args: &[Value]) -> Result<u64> {
        exec(&self.0, sql, args)
    }

    fn query(&mut self, sql: &str, args: &[Value]) -> Result<Vec<Row>> {
        fetch(&self.0, sql, args)
    }
}

impl Transaction for SqliteTx<'_> {
    fn commit(self: Box<Self>) -> Result<()> {
        self.0.commit().map_err(Error::backend)
    }
}

/// Statements are prepared through rusqlite's own cache, so the hot path --
/// one `INSERT` per task in a batch, one per event in a sync -- reuses a
/// prepared statement rather than reparsing the SQL every row.
fn exec(conn: &rusqlite::Connection, sql: &str, args: &[Value]) -> Result<u64> {
    let mut stmt = conn.prepare_cached(sql).map_err(Error::backend)?;
    let n =
        stmt.execute(rusqlite::params_from_iter(args.iter().map(Bind))).map_err(Error::backend)?;
    Ok(n as u64)
}

fn fetch(conn: &rusqlite::Connection, sql: &str, args: &[Value]) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare_cached(sql).map_err(Error::backend)?;
    let columns = stmt.column_count();
    let mut rows =
        stmt.query(rusqlite::params_from_iter(args.iter().map(Bind))).map_err(Error::backend)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(Error::backend)? {
        let mut values = Vec::with_capacity(columns);
        for i in 0..columns {
            values.push(from_sqlite(row.get_ref(i).map_err(Error::backend)?));
        }
        out.push(Row(values));
    }
    Ok(out)
}

/// A [`Value`] on its way into rusqlite.
struct Bind<'a>(&'a Value);

impl rusqlite::ToSql for Bind<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        use rusqlite::types::Value as V;
        Ok(match self.0 {
            Value::Null => ToSqlOutput::Owned(V::Null),
            // SQLite has no boolean type; it has always stored these as
            // 0 and 1, and the schema declares the columns INTEGER to match.
            Value::Bool(b) => ToSqlOutput::Owned(V::Integer(i64::from(*b))),
            Value::Int(n) => ToSqlOutput::Owned(V::Integer(*n)),
            Value::Real(f) => ToSqlOutput::Owned(V::Real(*f)),
            Value::Text(s) => ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes())),
            Value::Bytes(b) => ToSqlOutput::Borrowed(ValueRef::Blob(b)),
        })
    }
}

/// A rusqlite value on its way out.
///
/// Text that is not UTF-8 comes back as bytes rather than as an error: this
/// crate only ever writes UTF-8 into a text column, so the case cannot arise
/// from its own writes, and turning a damaged byte into a decode failure
/// three layers down would be a worse report than the type mismatch the
/// caller gets instead.
fn from_sqlite(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => Value::Int(n),
        ValueRef::Real(f) => Value::Real(f),
        ValueRef::Text(t) => match std::str::from_utf8(t) {
            Ok(s) => Value::Text(s.to_string()),
            Err(_) => Value::Bytes(t.to_vec()),
        },
        ValueRef::Blob(b) => Value::Bytes(b.to_vec()),
    }
}

#[cfg(test)]
mod tests;

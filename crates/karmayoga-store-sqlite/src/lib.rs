//! SQLite driver for the SQL storage backend.
//!
//! This is the default backend: one database file for records, a directory
//! of chunk-encrypted files for media.
//!
//! ```text
//!   store/
//!     karmayoga.db     journals + entries, tasks + time, calendars, library
//!     media/           attachment payloads (see karmayoga_core::blobstore)
//! ```
//!
//! # What is in here, and what is not
//!
//! Not: the schema, the queries, the cascades, or the decision about which
//! columns stay in the clear. All of that is in
//! [`karmayoga_store_sql`](karmayoga_store_sql), written once and shared with
//! every other database Karma Yoga speaks — which is what makes "the same
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

use karmayoga_core::backup::{
    BackupJob, StagedFile, is_sqlite_file, list_files, might_be_database,
};
use karmayoga_core::error::{Error, Result};
use karmayoga_core::store::{IntegrityJob, JournalStore, StoreContext, StoreFactory};
use karmayoga_store_sql::conn::{Connection, Row, Sql, Transaction, Value};
use karmayoga_store_sql::dialect::Dialect;
use karmayoga_store_sql::schema::VersionStore;
use karmayoga_store_sql::{Driver, Media, SqlStore};
use rusqlite::types::{ToSqlOutput, ValueRef};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const BACKEND_ID: &str = "sqlite";
pub(crate) const DB_FILENAME: &str = "karmayoga.db";
pub(crate) const MEDIA_DIRNAME: &str = "media";

/// Registers this backend with a [`karmayoga_core::BackendRegistry`].
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
        let db_path = db_file(&ctx);
        let conn = connect(&db_path)?;
        let media = Media::files(ctx.root.join(MEDIA_DIRNAME), ctx.cipher.clone())?;
        SqlStore::open(Arc::new(SqliteDriver { db_path }), Box::new(SqliteConn(conn)), media, &ctx)
    }
}

/// The database file, carried across from the name it had before the rename
/// from Every Day ([`karmayoga_core::legacy::SQLITE_DB_FILENAME`]).
///
/// Renamed only by a process that holds the vault (see
/// [`StoreContext::exclusive`]); one that does not reads the file where it
/// is. And only once nothing lives in a file named after the old one: the
/// write-ahead log is folded into the database first, and the rename is put
/// off -- the old name kept for this open -- if that cannot finish, or if a
/// `-wal` or `-shm` is still there afterwards, which means another
/// connection somewhere still has the file open. A later open tries again.
///
/// A relative link is left at the old name, as the folders get one: an old
/// build started again opens the same database through it rather than
/// creating an empty one beside it. SQLite follows the link and keeps its
/// `-wal` and `-shm` beside the real file, so both builds share one log.
fn db_file(ctx: &StoreContext) -> PathBuf {
    let current = ctx.root.join(DB_FILENAME);
    let old = ctx.root.join(karmayoga_core::legacy::SQLITE_DB_FILENAME);
    if current.exists() || !old.exists() {
        return current;
    }
    if !ctx.exclusive {
        return old;
    }
    match rename_legacy_db(&old, &current) {
        Ok(()) => {
            tracing::info!(from = %old.display(), to = %current.display(), "renamed the vault's database");
            current
        }
        Err(why) => {
            tracing::warn!(path = %old.display(), "kept the database's old name for now: {why}");
            old
        }
    }
}

fn rename_legacy_db(old: &Path, current: &Path) -> std::result::Result<(), String> {
    // Built from the path's own bytes, not its display form, which is lossy
    // for a path that is not UTF-8 and would check the wrong files.
    let side = |suffix: &str| {
        let mut name = old.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    {
        let conn = rusqlite::Connection::open(old).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
        // `busy` is 1 when a reader elsewhere kept the log from being
        // emptied; the rename waits for a quieter open.
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        if busy != 0 {
            return Err("another connection is reading it".into());
        }
    }
    // The last connection to close deletes both; either still being there
    // means this was not the last.
    if side("-wal").exists() || side("-shm").exists() {
        return Err("another connection still has it open".into());
    }
    std::fs::rename(old, current).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if let Err(e) = std::os::unix::fs::symlink(DB_FILENAME, old) {
        tracing::warn!(path = %old.display(), "no link left at the database's old name: {e}");
    }
    Ok(())
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
    /// cross-check. That is the right trade for something that runs every
    /// time a vault is opened -- torn pages are what a bad shutdown
    /// produces, and they are exactly what this catches.
    ///
    /// A fraction is still every page, though, and that is why it no longer
    /// runs *in* the unlock: see
    /// [`SqliteDriver::background_integrity_check`].
    fn check_integrity(&self, conn: &mut dyn Sql) -> Result<Vec<String>> {
        quick_check(conn)
    }

    /// The same `quick_check`, on a connection of its own that `cancel`
    /// interrupts mid-scan.
    ///
    /// Not the write connection, which is where [`Driver::check_integrity`]
    /// runs and where twenty seconds of reading would hold every save.
    /// Not a pooled reader either, which belongs to the store and could not
    /// outlive it. A connection opened here, and moved into the job, can.
    ///
    /// Opened now, on the caller's thread, rather than inside the job: a
    /// path that cannot be opened is then reported to whoever asked, and
    /// the job never opens anything by name -- which matters if the vault's
    /// directory is being removed by the time the job's thread gets going.
    ///
    /// Stopped through a progress handler rather than `sqlite3_interrupt`,
    /// because an interrupt sent before the statement starts is cleared when
    /// it does: a vault closed in the moment after an unlock would wait out
    /// the whole scan. The flag is read throughout, including between the
    /// pages of the check itself.
    fn background_integrity_check(&self, cancel: Arc<AtomicBool>) -> Result<Option<IntegrityJob>> {
        let conn = connect(&self.db_path)?;
        conn.progress_handler(CANCEL_CHECK_EVERY, Some(move || cancel.load(Ordering::Relaxed)))
            .map_err(Error::backend)?;
        let mut conn = SqliteConn(conn);
        Ok(Some(Box::new(move || quick_check(&mut conn))))
    }

    /// `VACUUM INTO`, plus a copy of everything else the store keeps.
    ///
    /// `VACUUM INTO` runs inside a read transaction, so the file it produces
    /// is a point-in-time snapshot even while the app keeps writing -- and it
    /// is a plain database, not a dump, so the backup directory opens as a
    /// vault without a restore step. Copying `karmayoga.db` with the
    /// filesystem would do neither: it would race the WAL and land a torn
    /// page.
    ///
    /// Everything else is copied after the database, which is the safe
    /// order. A blob or a mail pack present in the copy that nothing
    /// references is reclaimed by the next GC or compaction; a record
    /// referencing one that was missed would be an attachment, or a message,
    /// lost in the backup. That is all of it -- the media, mail's packs, a
    /// chat link's own store -- not only the media, which is all this used
    /// to copy: a backup made before that left every raw message behind.
    /// Another database among them (a chat link's) is copied through SQLite
    /// as this one is, for the same reason.
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

        for file in list_files(root, "", &is_the_database_or_a_log)? {
            let dest = dir.join(&file.path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            if might_be_database(&file.path) && is_sqlite_file(&file.source) {
                copy_database(&file.source, &dest)?;
            } else {
                match std::fs::copy(&file.source, &dest) {
                    Ok(_) => {}
                    // Collected or compacted since it was listed: nothing
                    // references it any more, or will once that finishes.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::io(&file.source, e)),
                }
            }
        }
        karmayoga_core::fsutil::sync_dir(dir);
        Ok(())
    }

    /// A consistent copy of the database through SQLite's backup API, and
    /// every other file under the store listed to be read in place -- any
    /// other database among them (a chat link's) copied the same way.
    ///
    /// The backup API rather than `VACUUM INTO`, which [`snapshot`] uses,
    /// because of what happens next: a backup cuts the copy into chunks and
    /// keeps only those no earlier copy had. The backup API copies the file
    /// page for page, so a day of mail moves only the pages it touched and
    /// most chunks are the ones already kept; `VACUUM INTO` rebuilds the
    /// file, and moves every page after the first change. It copies in one
    /// step, so it reads one point in time, on a connection of its own that
    /// only reads -- in WAL mode the app's writes carry on meanwhile.
    ///
    /// [`snapshot`]: Driver::snapshot
    fn backup_job(&self, root: &Path) -> Result<BackupJob> {
        let db_path = self.db_path.clone();
        let root = root.to_path_buf();
        Ok(Box::new(move |staging: &Path| stage_for_backup(&db_path, &root, staging)))
    }
}

/// What [`SqliteDriver::backup_job`]'s job does.
fn stage_for_backup(db_path: &Path, root: &Path, staging: &Path) -> Result<Vec<StagedFile>> {
    let staged = |path: String, file: PathBuf| -> Result<StagedFile> {
        let size = std::fs::metadata(&file).map_err(|e| Error::io(&file, e))?.len();
        Ok(StagedFile { path, source: file, size, modified_ns: None, database: true })
    };
    let mut files = Vec::new();
    let db_copy = staging.join(STORE_PREFIX).join(DB_FILENAME);
    // The copy is as large as the database and its log together. Said up
    // front, in numbers, rather than as a disk filling half way through.
    let needed = [db_path.to_path_buf(), sibling(db_path, "-wal")]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum::<u64>();
    if let Some(free) = karmayoga_core::fsutil::available_space(staging)
        && free < needed + needed / 20
    {
        return Err(Error::Invalid(format!(
            "a backup copies the database first, and that needs {} free on the vault's disk; \
             there is {}",
            gigabytes(needed),
            gigabytes(free)
        )));
    }
    copy_database(db_path, &db_copy)?;
    files.push(staged(format!("{STORE_PREFIX}/{DB_FILENAME}"), db_copy)?);

    for file in list_files(root, STORE_PREFIX, &|path| {
        is_the_database_or_a_log(
            path.strip_prefix(STORE_PREFIX).unwrap_or(path).trim_start_matches('/'),
        )
    })? {
        if might_be_database(&file.path) && is_sqlite_file(&file.source) {
            let copy = staging.join(&file.path);
            copy_database(&file.source, &copy)?;
            files.push(staged(file.path, copy)?);
        } else {
            files.push(file);
        }
    }
    Ok(files)
}

/// `path` with `suffix` on the end of its file name: a database's `-wal`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// What a backup's paths are prefixed with: the store's own directory under
/// the vault's.
const STORE_PREFIX: &str = "store";

/// The database under either of its names, and any database's log or
/// journal, by a path relative to the store's directory. Each database is
/// copied through SQLite, which takes in its log; a log copied as a file
/// would be a second, torn version of the same pages.
fn is_the_database_or_a_log(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path == DB_FILENAME
        || path == karmayoga_core::legacy::SQLITE_DB_FILENAME
        || name.ends_with("-wal")
        || name.ends_with("-shm")
        || name.ends_with("-journal")
}

/// Copy the SQLite database at `from` to `to` -- a new file -- as it stands
/// at one moment, through SQLite's backup API, in one step on a read-only
/// connection of its own. The copy is left as one file with no log beside
/// it, ready to be read as bytes.
fn copy_database(from: &Path, to: &Path) -> Result<()> {
    use rusqlite::OpenFlags;
    use rusqlite::backup::{Backup, StepResult};
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let source = rusqlite::Connection::open_with_flags(
        from,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(Error::backend)?;
    source.busy_timeout(std::time::Duration::from_secs(5)).map_err(Error::backend)?;
    let mut dest = rusqlite::Connection::open(to).map_err(Error::backend)?;
    {
        let backup = Backup::new(&source, &mut dest).map_err(Error::backend)?;
        let mut waits = 0;
        loop {
            match backup.step(-1).map_err(Error::backend)? {
                StepResult::Done => break,
                StepResult::More => {}
                // Somebody holds a lock the copy has to wait out -- a
                // checkpoint, usually. A minute of that is not waiting any
                // more.
                _ if waits < 600 => {
                    waits += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                _ => {
                    return Err(Error::Invalid(format!(
                        "{} stayed busy for a minute; try the backup again",
                        from.display()
                    )));
                }
            }
        }
    }
    // One file, no log: the copy is read as bytes next.
    dest.pragma_update(None, "journal_mode", "DELETE").map_err(Error::backend)?;
    drop(dest);
    Ok(())
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

/// Run `PRAGMA quick_check` and keep what is wrong.
fn quick_check(conn: &mut dyn Sql) -> Result<Vec<String>> {
    let rows = conn.query("PRAGMA quick_check", &[])?;
    // A healthy database answers with the single row "ok".
    rows.into_iter().map(|r| r.text(0)).filter(|r| !matches!(r, Ok(s) if s == "ok")).collect()
}

/// How often a background integrity check looks at its cancel flag: every
/// this many b-tree pages of the scan, or virtual-machine steps outside it.
///
/// Pages, not bytes. A mail body is mostly overflow pages chained off the
/// one that holds its row, and the chain is walked between two looks, so a
/// hundred pages here can be a few megabytes of reading -- tens of
/// milliseconds from a cold disk, which is how long closing a vault waits.
const CANCEL_CHECK_EVERY: std::ffi::c_int = 100;

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

#[cfg(test)]
mod keyset_tests;

#[cfg(test)]
mod mail_scale;

#[cfg(test)]
mod scale;

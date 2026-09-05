//! SQLite storage backend.
//!
//! This is the default backend: one database file for records, a directory
//! of chunk-encrypted files for media.
//!
//! ```text
//!   store/
//!     everyday.db      journals + entries
//!     media/           attachment payloads (see everyday_core::blobstore)
//! ```
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
//!
//! Titles, bodies, tags, locations, attachments and file names are all
//! sealed. Someone with the database file learns *that* you journalled on 14
//! July 2024 and never what you wrote. This is the same trade Day One makes,
//! and it is what allows date-range queries and pagination to run as index
//! scans rather than decrypting the entire vault on every keystroke.
//!
//! If that trade is not acceptable, the abstraction is the answer: a backend
//! that seals the index columns too — at the cost of full scans — plugs in
//! without the rest of the app noticing.

use everyday_core::blobstore::FileBlobStore;
use everyday_core::error::{Error, Result};
use everyday_core::id::{BlobId, EntryId, JournalId};
use everyday_core::model::{Entry, EntrySummary, Journal};
use everyday_core::store::{
    Capabilities, EntryQuery, JournalStore, SortOrder, StoreContext, StoreFactory, StoreStats,
    entry_aad, journal_aad,
};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::sync::{Arc, Mutex};

pub const BACKEND_ID: &str = "sqlite";
const DB_FILENAME: &str = "everyday.db";
const MEDIA_DIRNAME: &str = "media";
const SCHEMA_VERSION: i64 = 1;

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

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 =
        conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(Error::backend)?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch(
        r#"
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
        "#,
    )
    .map_err(Error::backend)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(Error::backend)?;
    Ok(())
}

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
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::crypto::{AeadCipher, Cipher, NullCipher, SecretKey};
    use everyday_core::store::conformance;
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

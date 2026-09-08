//! The journal domain: journals, entries, blobs and housekeeping.
//!
//! This is [`JournalStore`], the trait every backend must implement. The
//! optional domains are in `tasks.rs`, `calendars.rs`, `library.rs`,
//! `trackers.rs` and `agent.rs`, mirroring the split
//! `everyday_core::store` already makes between the trait and its
//! siblings.

use everyday_core::error::{Error, Result};
use everyday_core::id::{BlobId, EntryId, JournalId};
use everyday_core::model::{Entry, EntrySummary, Journal};
use everyday_core::store::agent::AgentStore;
use everyday_core::store::calendars::CalendarStore;
use everyday_core::store::library::LibraryStore;
use everyday_core::store::tasks::TaskStore;
use everyday_core::store::trackers::TrackerStore;
use everyday_core::store::{
    Capabilities, EntryQuery, JournalStore, SortOrder, StoreStats, journal_aad,
};
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::{BACKEND_ID, SqliteStore, to_us};

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
            library: true,
            trackers: true,
            agent: true,
        }
    }

    fn tasks(&self) -> Option<&dyn TaskStore> {
        Some(self)
    }

    fn calendars(&self) -> Option<&dyn CalendarStore> {
        Some(self)
    }

    fn library(&self) -> Option<&dyn LibraryStore> {
        Some(self)
    }

    fn trackers(&self) -> Option<&dyn TrackerStore> {
        Some(self)
    }

    fn agent(&self) -> Option<&dyn AgentStore> {
        Some(self)
    }

    // ---- journals -------------------------------------------------------

    fn list_journals(&self) -> Result<Vec<Journal>> {
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
        conn.execute(
            "INSERT INTO journals (id, sort_order, updated_us, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET sort_order = ?2, updated_us = ?3, data = ?4",
            params![j.id.to_string(), j.sort_order, to_us(j.updated_at), sealed],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_journal(&self, id: JournalId) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(Error::backend)?;
        tx.execute("DELETE FROM entries WHERE journal_id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        // The readings too. Their definitions live inside the journal record
        // about to be deleted, so leaving them would strand rows whose
        // meaning is gone -- numbers against a tracker id nothing can name.
        tx.execute("DELETE FROM readings WHERE journal_id = ?1", params![id.to_string()])
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

        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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

    /// One statement, so the check and the write cannot be separated.
    ///
    /// The shared default in `everyday_core::store` reads, compares and then
    /// writes, which leaves a window another *process* can slip through --
    /// and cross-process is exactly the case this guards. Both forms below
    /// are conditional in SQL and report the conflict from the row count, so
    /// there is no window at all.
    fn put_entry_if(&self, e: &Entry, expect: Option<jiff::Timestamp>) -> Result<()> {
        let (data, summary) = self.seal_entry(e)?;
        let conn = self.conn();

        let changed = match expect {
            // Updating: only if `updated_us` is still what the caller read.
            // A row that has moved on, or has been deleted, matches nothing
            // and changes nothing.
            Some(want) => conn
                .execute(
                    "UPDATE entries SET
                        journal_id = ?2, local_date = ?3, created_us = ?4, updated_us = ?5,
                        starred = ?6, pinned = ?7, data = ?8, summary = ?9
                     WHERE id = ?1 AND updated_us = ?10",
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
                        to_us(want),
                    ],
                )
                .map_err(Error::backend)?,
            // Creating: `OR IGNORE` turns the primary-key clash into zero
            // rows rather than an error, so both branches report a conflict
            // the same way.
            None => conn
                .execute(
                    "INSERT OR IGNORE INTO entries
                        (id, journal_id, local_date, created_us, updated_us,
                         starred, pinned, data, summary)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
                .map_err(Error::backend)?,
        };

        if changed == 0 {
            return Err(Error::Conflict { kind: "entry" });
        }
        Ok(())
    }

    fn delete_entry(&self, id: EntryId) -> Result<()> {
        // Readings survive the entry they were logged beside, and are
        // detached from it rather than deleted with it. Deleting the
        // paragraph you wrote about a run does not undo the run, and the
        // number is the part a year of charts is made of. What must not
        // survive is the *pointer*: a reading naming an entry that is gone
        // is a link the next feature to follow it would trip over.
        self.detach_readings_from(id)?;
        let conn = self.conn();
        conn.execute("DELETE FROM entries WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }

    fn all_entries(&self) -> Result<Vec<Entry>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, data FROM entries ORDER BY local_date DESC, created_us DESC")
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

    fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        Ok(self.blobs.age_of(id))
    }

    fn delete_blob(&self, id: BlobId) -> Result<()> {
        self.blobs.delete(id)
    }

    fn list_blobs(&self) -> Result<Vec<BlobId>> {
        self.blobs.list()
    }

    // ---- housekeeping ---------------------------------------------------

    fn stats(&self) -> Result<StoreStats> {
        let conn = self.conn();
        let journals: i64 = conn
            .query_row("SELECT COUNT(*) FROM journals", [], |r| r.get(0))
            .map_err(Error::backend)?;
        let entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .map_err(Error::backend)?;
        drop(conn);
        let (blobs, blob_bytes) = self.blobs.stats()?;
        Ok(StoreStats { journals: journals as u64, entries: entries as u64, blobs, blob_bytes })
    }

    fn flush(&self) -> Result<()> {
        let conn = self.conn();
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").map_err(Error::backend)?;
        Ok(())
    }

    /// `PRAGMA quick_check`, which is the useful three quarters of
    /// `integrity_check` at a fraction of the cost: it verifies page
    /// structure and record sanity but skips the index-versus-table
    /// cross-check. That is the right trade for something that runs on
    /// unlock -- torn pages are what a bad shutdown produces, and they are
    /// exactly what this catches.
    fn check_integrity(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("PRAGMA quick_check").map_err(Error::backend)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<Vec<String>>>()
            .map_err(Error::backend)?;
        // A healthy database answers with the single row "ok".
        Ok(rows.into_iter().filter(|r| r != "ok").collect())
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
    fn snapshot(&self, dir: &std::path::Path) -> Result<()> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let db = dir.join(crate::DB_FILENAME);
        if db.exists() {
            return Err(Error::Invalid(format!(
                "{} already holds a vault; back up into an empty directory",
                dir.display()
            )));
        }

        {
            let conn = self.conn();
            // Bound as a parameter: a backup path can contain a quote.
            conn.execute("VACUUM INTO ?1", params![db.to_string_lossy()])
                .map_err(Error::backend)?;
        }

        everyday_core::fsutil::copy_tree(
            &self.root.join(crate::MEDIA_DIRNAME),
            &dir.join(crate::MEDIA_DIRNAME),
        )?;
        everyday_core::fsutil::sync_dir(dir);
        Ok(())
    }
}

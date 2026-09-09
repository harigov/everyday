//! The journal domain: journals, entries, blobs and housekeeping.
//!
//! This is [`JournalStore`], the trait every backend must implement. The five
//! optional domains are in `tasks.rs`, `calendars.rs`, `library.rs`,
//! `trackers.rs` and `agent.rs`, mirroring the split `everyday_core::store`
//! already makes between the trait and its siblings.

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

use crate::conn::{SqlExt, Value};
use crate::{SqlStore, to_us, vals};

impl JournalStore for SqlStore {
    fn backend(&self) -> &'static str {
        self.driver.backend_id()
    }

    fn capabilities(&self) -> Capabilities {
        SqlStore::capabilities(self)
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
        let rows =
            self.conn().records("SELECT id, data FROM journals ORDER BY sort_order, id", &[])?;
        self.collect(rows, journal_aad)
    }

    fn get_journal(&self, id: JournalId) -> Result<Journal> {
        let sealed = self
            .conn()
            .sealed("SELECT data FROM journals WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("journal", id))?;
        self.unseal(&journal_aad(id), &sealed)
    }

    fn put_journal(&self, j: &Journal) -> Result<()> {
        let sealed = self.seal(&journal_aad(j.id), j)?;
        self.conn().execute(
            "INSERT INTO journals (id, sort_order, updated_us, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET sort_order = ?2, updated_us = ?3, data = ?4",
            &vals![j.id.to_string(), j.sort_order, to_us(j.updated_at), sealed],
        )?;
        Ok(())
    }

    fn delete_journal(&self, id: JournalId) -> Result<()> {
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        let args = vals![id.to_string()];
        tx.execute("DELETE FROM entries WHERE journal_id = ?1", &args)?;
        // The readings too. Their definitions live inside the journal record
        // about to be deleted, so leaving them would strand rows whose
        // meaning is gone -- numbers against a tracker id nothing can name.
        tx.execute("DELETE FROM readings WHERE journal_id = ?1", &args)?;
        tx.execute("DELETE FROM journals WHERE id = ?1", &args)?;
        tx.commit()
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
        let mut args: Vec<Value> = Vec::new();

        if let Some(j) = query.journal_id {
            args.push(Value::Text(j.to_string()));
            sql.push_str(&format!(" AND journal_id = ?{}", args.len()));
        }
        if let Some(from) = query.from {
            args.push(Value::Text(from.to_string()));
            sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Value::Text(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(starred) = query.starred {
            args.push(Value::Bool(starred));
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
            // Neither database honours an OFFSET without a LIMIT, and they
            // spell "no limit" differently. See `Dialect::limit_offset`.
            sql.push_str(&self.dialect.limit_offset(query.limit, query.offset));
        }

        let rows = self.conn().records(&sql, &args)?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, sealed) in rows {
            let id = EntryId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?;
            out.push(self.open_summary(id, &sealed)?);
        }

        Ok(if needs_memory_pass { query.apply(out) } else { out })
    }

    fn get_entry(&self, id: EntryId) -> Result<Entry> {
        let sealed = self
            .conn()
            .sealed("SELECT data FROM entries WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("entry", id))?;
        self.open_entry(id, &sealed)
    }

    fn put_entry(&self, e: &Entry) -> Result<()> {
        let (data, summary) = self.seal_entry(e)?;
        self.conn().execute(
            "INSERT INTO entries
                (id, journal_id, local_date, created_us, updated_us, starred, pinned, data, summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (id) DO UPDATE SET
                journal_id = ?2, local_date = ?3, created_us = ?4, updated_us = ?5,
                starred = ?6, pinned = ?7, data = ?8, summary = ?9",
            &vals![
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
        )?;
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

        let changed = match expect {
            // Updating: only if `updated_us` is still what the caller read.
            // A row that has moved on, or has been deleted, matches nothing
            // and changes nothing.
            Some(want) => self.conn().execute(
                "UPDATE entries SET
                    journal_id = ?2, local_date = ?3, created_us = ?4, updated_us = ?5,
                    starred = ?6, pinned = ?7, data = ?8, summary = ?9
                 WHERE id = ?1 AND updated_us = ?10",
                &vals![
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
            )?,
            // Creating: `DO NOTHING` turns the primary-key clash into zero
            // rows rather than an error, so both branches report a conflict
            // the same way.
            None => self.conn().execute(
                "INSERT INTO entries
                    (id, journal_id, local_date, created_us, updated_us,
                     starred, pinned, data, summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (id) DO NOTHING",
                &vals![
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
            )?,
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
        self.conn().execute("DELETE FROM entries WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }

    fn all_entries(&self) -> Result<Vec<Entry>> {
        let rows = self.conn().records(
            "SELECT id, data FROM entries ORDER BY local_date DESC, created_us DESC",
            &[],
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, sealed) in rows {
            let id = EntryId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?;
            out.push(self.open_entry(id, &sealed)?);
        }
        Ok(out)
    }

    // ---- blobs ----------------------------------------------------------
    //
    // Where these land depends on the backend, not on the domain: a local
    // database keeps them in files beside itself and a remote one keeps them
    // in a table, because attachments on one laptop are not in a vault two
    // machines share. Both hold the same sealed bytes. See `blobs.rs`.

    fn put_blob(&self, bytes: &[u8]) -> Result<BlobId> {
        self.blob_put(bytes)
    }

    fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
        self.blob_get(id)
    }

    fn blob_len(&self, id: BlobId) -> Result<u64> {
        SqlStore::blob_len(self, id)
    }

    fn get_blob_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.blob_range(id, offset, len)
    }

    fn has_blob(&self, id: BlobId) -> Result<bool> {
        self.blob_has(id)
    }

    fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        SqlStore::blob_age(self, id)
    }

    fn delete_blob(&self, id: BlobId) -> Result<()> {
        self.blob_delete(id)
    }

    fn list_blobs(&self) -> Result<Vec<BlobId>> {
        self.blob_list()
    }

    // ---- housekeeping ---------------------------------------------------

    fn stats(&self) -> Result<StoreStats> {
        let (journals, entries) = {
            let mut conn = self.conn();
            (
                conn.scalar_i64("SELECT COUNT(*) FROM journals", &[])?,
                conn.scalar_i64("SELECT COUNT(*) FROM entries", &[])?,
            )
        };
        let (blobs, blob_bytes) = self.blob_stats()?;
        Ok(StoreStats { journals: journals as u64, entries: entries as u64, blobs, blob_bytes })
    }

    fn flush(&self) -> Result<()> {
        let mut conn = self.conn();
        self.driver.flush(conn.as_mut())
    }

    fn check_integrity(&self) -> Result<Vec<String>> {
        let mut conn = self.conn();
        self.driver.check_integrity(conn.as_mut())
    }

    fn snapshot(&self, dir: &std::path::Path) -> Result<()> {
        let mut conn = self.conn();
        self.driver.snapshot(conn.as_mut(), &self.root, dir)
    }
}

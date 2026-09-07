//! The journal domain: journals, entries, blobs and housekeeping.
//!
//! This is [`JournalStore`], the trait every backend must implement. The two
//! optional domains are in `tasks.rs` and `calendars.rs`, mirroring the split
//! `everyday_core::store` already makes between the trait and its two
//! siblings.

use everyday_core::error::{Error, Result};
use everyday_core::id::{BlobId, EntryId, JournalId};
use everyday_core::model::{Entry, EntrySummary, Journal};
use everyday_core::store::calendars::CalendarStore;
use everyday_core::store::tasks::TaskStore;
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

    fn delete_blob(&self, id: BlobId) -> Result<()> {
        self.blobs.delete(id)
    }

    fn list_blobs(&self) -> Result<Vec<BlobId>> {
        self.blobs.list()
    }

    // ---- housekeeping ---------------------------------------------------

    fn stats(&self) -> Result<StoreStats> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").map_err(Error::backend)?;
        Ok(())
    }
}

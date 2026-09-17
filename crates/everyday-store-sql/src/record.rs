//! One shape, four operations, written once.
//!
//! Every table in this crate holds the same three things: an `id TEXT`
//! primary key, a handful of clear columns an index is built from, and a
//! sealed `data BLOB` that is the record in full. That repetition used to be
//! paid for at every call site — a `.sealed("SELECT data FROM t WHERE id =
//! ?1")... ok_or_else(not_found)... unseal(aad, ...)` written out by hand at
//! roughly twenty get-by-id sites, and an `INSERT ... ON CONFLICT (id) DO
//! UPDATE SET <every column again>` written out at roughly twenty-three
//! upserts, one column list typed twice each time it appeared. [`Record`]
//! names the shape once, and [`SqlStore::get`], [`SqlStore::upsert`],
//! [`SqlStore::upsert_many`] and [`SqlStore::delete_by_id`] are what a table
//! gets for free by describing it.
//!
//! Not every table qualifies, and that is by design rather than an
//! oversight. A table whose write updates fewer columns on conflict than it
//! inserts — `conversations` leaves `created_us` alone, `messages` and
//! `memories` only ever touch `data` — cannot be expressed by a helper that
//! always rewrites every column, so those stay hand-written; see the
//! comments beside them in `agent.rs`. A table with a second sealed column
//! beside `data` — `entries` and `notes` both keep a `summary` a list draws
//! without opening the whole record — cannot be filled in by [`columns`]
//! either, because sealing it needs the store's cipher and `columns` is a
//! method on the record alone. Both of those keep their own `put`, and use
//! only the generic `get`.

use crate::conn::{Sql, SqlExt, Value};
use crate::purpose::set_purpose;
use crate::{SqlStore, vals};
use everyday_core::error::{Error, Result};
use everyday_core::purpose::Purpose;
use everyday_core::record::{RecordDescriptor, RecordKind};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Display;
use std::str::FromStr;

/// A record with the id/clear-columns/sealed-payload shape every table in
/// this crate uses.
///
/// The identity half — `KIND`, `Id`, `id()`, `aad()` — is
/// [`RecordDescriptor`], in `everyday-core`, so that a record's kind and its
/// aad label are named once for the whole workspace rather than once per
/// crate. This trait adds only what is specific to being a *SQL* table: the
/// one it lives in, its clear columns, and where its purpose pointer (for
/// the tables that carry one) comes from.
pub(crate) trait Record: RecordDescriptor + Serialize + DeserializeOwned {
    /// The table this record lives in.
    const TABLE: &'static str;

    /// Every clear column but `id` and `data`, in the order the table
    /// declares them. [`upsert_stmt`] adds `id` at the front and `data` at
    /// the back, so this is the one place a table's column order is ever
    /// written down.
    fn columns(&self) -> Vec<(&'static str, Value)>;

    /// The purpose kind this table's rows file under, for the handful that
    /// carry one. `None` — the default — for every table that does not.
    fn purpose_kind() -> Option<RecordKind> {
        None
    }

    /// This record's own purpose, for [`upsert_many`](SqlStore::upsert_many)
    /// to write alongside it. Only meaningful where
    /// [`purpose_kind`](Record::purpose_kind) is `Some`; a table whose
    /// purpose is computed rather than stored on the record itself — a
    /// calendar's is derived from its `role_id` — answers `None` here and
    /// keeps its own `set_purpose` call instead.
    fn purpose(&self) -> Option<&Purpose> {
        None
    }
}

/// The `INSERT ... ON CONFLICT (id) DO UPDATE SET ...` text and its
/// arguments for one record, in `id, <Record::columns>, data` order.
///
/// A free function rather than a method, so that a write which has to share
/// a transaction with other statements can call it directly instead of going
/// through [`SqlStore::upsert`], which owns its own. `replace_events` is the
/// one place in this crate that needs that: it deletes a calendar's events
/// and reinserts them in a single transaction, and reaches for this to build
/// each `INSERT` rather than opening a transaction of its own for every row.
pub(crate) fn upsert_stmt<R: Record>(record: &R, sealed: Vec<u8>) -> (String, Vec<Value>) {
    let mut cols = record.columns();
    cols.insert(0, ("id", Value::Text(record.id().to_string())));
    cols.push(("data", Value::Bytes(sealed)));

    let names: Vec<&str> = cols.iter().map(|(name, _)| *name).collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|n| format!("?{n}")).collect();
    // Every column but `id`, which is the conflict target and so is never
    // itself reassigned.
    let sets: Vec<String> = names[1..]
        .iter()
        .zip(&placeholders[1..])
        .map(|(name, p)| format!("{name} = {p}"))
        .collect();

    let sql = format!(
        "INSERT INTO {table} ({cols}) VALUES ({phs}) ON CONFLICT (id) DO UPDATE SET {sets}",
        table = R::TABLE,
        cols = names.join(", "),
        phs = placeholders.join(", "),
        sets = sets.join(", "),
    );
    (sql, cols.into_iter().map(|(_, v)| v).collect())
}

/// A batch of [`upsert_batched`](SqlStore::upsert_batched) closes at four
/// megabytes of sealed payload, or [`BATCH_MAX_ROWS`], whichever comes
/// first.
///
/// Picked against the sync engine's own words for the shape of a first
/// sync — "headers for every message, in batches of a few hundred,
/// committed per batch" — rather than against a measured lock-hold time,
/// because none exists yet: this is groundwork for the caller that will
/// measure it. Four megabytes is small enough that even a slow disk commits
/// it well under the batch cadence a sync engine already works in (a batch
/// every few hundred milliseconds), and large enough that the row cap below
/// is almost always what actually closes a batch of ordinary-sized records.
const BATCH_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The other half of [`BATCH_MAX_BYTES`]: "a few hundred", named exactly.
const BATCH_MAX_ROWS: usize = 500;

/// Split sealed rows into batches, closing one whenever the next row would
/// push it past `max_bytes` or `max_rows` — never splitting a single row
/// across two batches, so a row bigger than `max_bytes` on its own is still
/// one batch rather than a reason to fail.
fn batches_by_size<R>(
    sealed: Vec<(&R, Vec<u8>)>,
    max_bytes: usize,
    max_rows: usize,
) -> Vec<Vec<(&R, Vec<u8>)>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0usize;
    for row in sealed {
        let row_bytes = row.1.len();
        if !current.is_empty() && (bytes + row_bytes > max_bytes || current.len() >= max_rows) {
            batches.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += row_bytes;
        current.push(row);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

impl SqlStore {
    /// One record by id, or [`Error::NotFound`] naming its kind.
    pub(crate) fn get<R: Record>(&self, id: R::Id) -> Result<R> {
        let sealed = self
            .read()
            .sealed(
                &format!("SELECT data FROM {} WHERE id = ?1", R::TABLE),
                &vals![id.to_string()],
            )?
            .ok_or_else(|| Error::not_found(R::KIND.not_found_noun(), id))?;
        self.unseal(&R::aad(id), &sealed)
    }

    /// Insert or replace one record. See
    /// [`upsert_many`](SqlStore::upsert_many), which this is a shorthand for.
    pub(crate) fn upsert<R: Record>(&self, record: &R) -> Result<()> {
        self.upsert_many(std::slice::from_ref(record))
    }

    /// Insert or replace many records in one transaction — what a re-ordered
    /// list, or moving several things to another parent at once, actually
    /// is. Each record's row is built once from [`Record::columns`], and its
    /// purpose pointer, for the tables that carry one, is written alongside
    /// it rather than in a transaction of its own.
    ///
    /// All or nothing, whatever the size: a board reorder that half landed
    /// is a board in an order nobody chose. A write too large to hold the
    /// writer through is [`upsert_batched`](SqlStore::upsert_batched)'s job,
    /// and choosing it is the caller saying it can resume.
    pub(crate) fn upsert_many<R: Record>(&self, records: &[R]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let sealed = self.seal_all(records)?;
        self.write_batch(sealed)
    }

    /// Insert or replace many records, committing every
    /// [`BATCH_MAX_BYTES`] or [`BATCH_MAX_ROWS`], whichever comes first.
    ///
    /// For a mailbox's first sync — headers "in batches of a few hundred,
    /// committed per batch" — which would otherwise hold the vault's single
    /// writer, and so block every other write, for as long as the whole call
    /// takes. Bytes alone would let a batch of tiny rows grow without limit;
    /// rows alone would let a handful of enormous bodies hold the lock for
    /// seconds.
    ///
    /// The trade is explicit: a call of several batches is not atomic as a
    /// whole, and a crash between batches leaves a prefix committed. That is
    /// safe only because `upsert_stmt` is `ON CONFLICT ... DO UPDATE`, so a
    /// replay is harmless, and only for a caller that already resumes from a
    /// cursor of its own. Anything that needs all-or-nothing calls
    /// [`upsert_many`](SqlStore::upsert_many).
    ///
    /// A single record larger than [`BATCH_MAX_BYTES`] is still its own
    /// batch rather than an error: the cap is a target for how long a batch
    /// takes to write, not a ceiling this refuses to cross.
    // First caller is the mail sync engine; until it lands, only the batch
    // splitter's tests exercise the arithmetic underneath.
    #[allow(dead_code)]
    pub(crate) fn upsert_batched<R: Record>(&self, records: &[R]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let sealed = self.seal_all(records)?;
        for batch in batches_by_size(sealed, BATCH_MAX_BYTES, BATCH_MAX_ROWS) {
            self.write_batch(batch)?;
        }
        Ok(())
    }

    /// Sealed before the lock is taken: encryption is the expensive part,
    /// and there is no reason to hold the connection through it.
    fn seal_all<'r, R: Record>(&self, records: &'r [R]) -> Result<Vec<(&'r R, Vec<u8>)>> {
        records.iter().map(|r| Ok((r, self.seal(&R::aad(r.id()), r)?))).collect()
    }

    /// One transaction on the writer for rows already sealed.
    fn write_batch<R: Record>(&self, rows: Vec<(&R, Vec<u8>)>) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (record, data) in rows {
            let (sql, args) = upsert_stmt(record, data);
            tx.execute(&sql, &args)?;
            if let Some(kind) = R::purpose_kind() {
                set_purpose(tx.as_mut(), kind, &record.id().to_string(), record.purpose())?;
            }
        }
        tx.commit()
    }

    /// `DELETE FROM table WHERE id = ?1`, on its own connection.
    ///
    /// For the tables whose delete really is that one statement. Anything
    /// with a cascade, a purpose pointer to forget, or a transaction it must
    /// share with other statements takes the write connection itself instead
    /// — seee the domain modules for those.
    pub(crate) fn delete_by_id<R: Record>(&self, id: R::Id) -> Result<u64> {
        self.write()
            .execute(&format!("DELETE FROM {} WHERE id = ?1", R::TABLE), &vals![id.to_string()])
    }

    /// Read every row `select_sql` names, let `f` change each one, and write
    /// every row it touched back — the read-modify-reseal loop
    /// `detach_readings_from`, `merge_trackers`, `detach_readings_in` and
    /// `mark_runs_seen` each wrote out by hand, with the same explanation of
    /// the reader-pool race attached to all four.
    ///
    /// # Why this takes the writer, and only the writer
    ///
    /// This is a read-modify-write, and the read has to be exactly as
    /// current as the write it is about to make. Taking the `SELECT` on a
    /// pooled reader would leave a window in which somebody else's write
    /// lands between the read and the reseal — and the reseal would then
    /// write back the payload this call decrypted, silently reverting their
    /// edit. So the whole sequence runs on the write connection, inside one
    /// transaction, which is also what makes the set of rewritten rows
    /// consistent with itself: a failure part way through leaves nothing
    /// half-changed. Callers pass the transaction in rather than this taking
    /// its own, so it can share one with whatever else the caller's delete
    /// or merge is doing.
    ///
    /// `select_sql` must select `id, data`. Every row it names is resealed
    /// and written back unconditionally, so a caller that wants to skip rows
    /// `f` did not actually change — `mark_runs_seen` skips a run that was
    /// already seen — has `f` leave them unchanged rather than asking this
    /// to notice.
    pub(crate) fn rewrite_each<R: Record>(
        &self,
        tx: &mut dyn Sql,
        select_sql: &str,
        args: &[Value],
        mut f: impl FnMut(&mut R),
    ) -> Result<u64>
    where
        <R::Id as FromStr>::Err: Display,
    {
        let rows = tx.records(select_sql, args)?;
        let mut n = 0u64;
        for (id, sealed) in rows {
            let id: R::Id =
                id.parse().map_err(|e: <R::Id as FromStr>::Err| Error::Invalid(e.to_string()))?;
            let mut record: R = self.unseal(&R::aad(id), &sealed)?;
            f(&mut record);
            let data = self.seal(&R::aad(id), &record)?;
            // Reuses the same INSERT text every other write in this crate
            // builds from `Record::columns` — every row this reads already
            // exists, so it always takes the `DO UPDATE` branch, which is an
            // `UPDATE` by another name.
            let (sql, sql_args) = upsert_stmt(&record, data);
            tx.execute(&sql, &sql_args)?;
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row stands in for `(&R, Vec<u8>)` without a real `Record` — the
    /// splitter only ever looks at `.1.len()`, so a bare byte count is
    /// enough to check it.
    fn row(bytes: usize) -> (&'static (), Vec<u8>) {
        (&(), vec![0u8; bytes])
    }

    #[test]
    fn a_batch_closes_on_whichever_cap_it_reaches_first() {
        // Three rows of 3 MB each: the byte cap (4 MB) closes a batch after
        // the first row, long before the row cap (500) would.
        let rows = vec![row(3_000_000), row(3_000_000), row(3_000_000)];
        let batches = batches_by_size(rows, 4_000_000, 500);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 1]);
    }

    #[test]
    fn small_rows_batch_by_count_instead() {
        let rows: Vec<_> = (0..1_203).map(|_| row(10)).collect();
        let batches = batches_by_size(rows, 4_000_000, 500);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [500, 500, 203]);
    }

    #[test]
    fn one_row_bigger_than_the_cap_is_still_one_batch() {
        let rows = vec![row(9_000_000)];
        let batches = batches_by_size(rows, 4_000_000, 500);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1, "a single oversized row is not split, or refused");
    }

    #[test]
    fn an_empty_input_makes_no_batches() {
        assert!(batches_by_size(Vec::<(&(), Vec<u8>)>::new(), 4_000_000, 500).is_empty());
    }

    #[test]
    fn every_row_lands_in_exactly_one_batch_in_order() {
        let rows: Vec<_> = (0..777).map(|n| row(n % 5000)).collect();
        let sizes: Vec<usize> = rows.iter().map(|r| r.1.len()).collect();
        let batches = batches_by_size(rows, 100_000, 50);
        let flattened: Vec<usize> = batches.into_iter().flatten().map(|r| r.1.len()).collect();
        assert_eq!(flattened, sizes, "batching must not reorder or drop a row");
    }
}

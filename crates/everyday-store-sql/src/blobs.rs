//! Attachments, for a store whose database is not on this machine.
//!
//! The default answer for media is a directory of sealed files beside the
//! database — [`FileBlobStore`](everyday_core::FileBlobStore) — and it is the
//! right answer whenever the database is a local file, for the reasons its
//! own module docs give: a phone video is three orders of magnitude bigger
//! than the entry that embeds it, and keeping it out of the database keeps
//! the database small, fast to open and cheap to back up.
//!
//! None of that survives the database moving to a server. A directory of
//! attachments on one laptop is not part of a vault that two machines open:
//! it is missing from the second one, and gone with the first. So a remote
//! backend keeps blobs in a `blobs` table, and pays the costs knowingly:
//!
//! * **A size ceiling.** Postgres refuses a single value over 1 GB, so
//!   [`Driver::max_blob_bytes`](crate::Driver::max_blob_bytes) reports a
//!   limit and the interface can say "too large" before the paste rather than
//!   after the upload.
//! * **A bigger database.** Which is the price of the attachments being in
//!   the vault at all.
//!
//! What it does *not* pay is a decryption cost or a seekability cost. The
//! bytes in the column are the identical container
//! [`everyday_core::blobstore`] writes to a file — same header, same
//! chunking, same per-chunk associated data — so a range request for the
//! middle of a video is `substr()` over two chunks, not a download of 400 MB.
//! It also means a blob is the same sequence of bytes in either backend,
//! which is what would make moving a vault between them a copy.
//!
//! The server never sees plaintext: sealing happens here, before the
//! `INSERT`.
//!
//! # One trait, two backings
//!
//! [`BlobBackend`] names the nine things this crate asks of wherever
//! attachments live. [`FileBlobStore`] already has all nine as inherent
//! methods; [`TableBlobs`] gives the `blobs`-table answer the same shape, so
//! `SqlStore`'s own nine methods are a one-line match each rather than a
//! `Media::Files(...) => ..., Media::Table => { ten more lines }` repeated
//! nine times.

use crate::conn::{Connection, Sql, SqlExt};
use crate::dialect::Dialect;
use crate::{Media, SqlStore, to_us, vals};
use everyday_core::blobstore::{self, FileBlobStore, Geometry, HEADER_LEN};
use everyday_core::error::{Error, Result};
use everyday_core::id::BlobId;

/// Create the `blobs` table if it is not already there.
///
/// Called on open rather than from a migration step — see
/// [`schema::blobs_table`](crate::schema::blobs_table) for why.
pub fn create_table(conn: &mut dyn Connection, dialect: Dialect) -> Result<()> {
    let mut tx = conn.begin()?;
    for sql in crate::schema::blobs_table(dialect) {
        tx.execute(&sql, &[])?;
    }
    tx.commit()
}

/// Wherever a blob's sealed bytes actually live: a directory of files, or a
/// `blobs` table. [`FileBlobStore`] and [`TableBlobs`] are the two answers.
trait BlobBackend {
    fn put(&self, bytes: &[u8]) -> Result<BlobId>;
    fn get(&self, id: BlobId) -> Result<Vec<u8>>;
    fn get_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>>;
    fn len_of(&self, id: BlobId) -> Result<u64>;
    fn has(&self, id: BlobId) -> Result<bool>;
    fn age_of(&self, id: BlobId) -> Result<Option<std::time::Duration>>;
    fn delete(&self, id: BlobId) -> Result<()>;
    fn list(&self) -> Result<Vec<BlobId>>;
    fn stats(&self) -> Result<(u64, u64)>;
}

impl BlobBackend for FileBlobStore {
    fn put(&self, bytes: &[u8]) -> Result<BlobId> {
        FileBlobStore::put(self, bytes)
    }
    fn get(&self, id: BlobId) -> Result<Vec<u8>> {
        FileBlobStore::get(self, id)
    }
    fn get_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        FileBlobStore::get_range(self, id, offset, len)
    }
    fn len_of(&self, id: BlobId) -> Result<u64> {
        FileBlobStore::len_of(self, id)
    }
    fn has(&self, id: BlobId) -> Result<bool> {
        Ok(FileBlobStore::has(self, id))
    }
    fn age_of(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        Ok(FileBlobStore::age_of(self, id))
    }
    fn delete(&self, id: BlobId) -> Result<()> {
        FileBlobStore::delete(self, id)
    }
    fn list(&self) -> Result<Vec<BlobId>> {
        FileBlobStore::list(self)
    }
    fn stats(&self) -> Result<(u64, u64)> {
        FileBlobStore::stats(self)
    }
}

/// The `blobs`-table answer to [`BlobBackend`], for a store with no local
/// disk of its own. Borrows the store rather than owning anything, since
/// what it needs -- the connections, the cipher, the driver's size limit --
/// all belong to it.
struct TableBlobs<'s>(&'s SqlStore);

impl BlobBackend for TableBlobs<'_> {
    fn put(&self, bytes: &[u8]) -> Result<BlobId> {
        let store = self.0;
        if let Some(max) = store.driver.max_blob_bytes()
            && bytes.len() as u64 > max
        {
            return Err(Error::Invalid(format!(
                "this attachment is {} bytes; the {} backend accepts up to {max}",
                bytes.len(),
                store.driver.backend_id(),
            )));
        }
        let id = BlobId::of(bytes);
        // Content-addressed: identical bytes, identical row. Checking first
        // also avoids sealing a 400 MB video to throw it away.
        if self.has(id)? {
            return Ok(id);
        }
        let sealed = blobstore::seal(store.cipher.as_ref(), id, bytes)?;
        // `DO NOTHING` rather than an error: two threads storing the same
        // photo is a race with one right answer, and it is the row that is
        // already there. Safe to run without checking again first: the
        // `has` above is only an optimisation to skip sealing a large
        // payload it already has, and if two callers race past it, this
        // statement -- not that check -- is what actually decides, and it
        // decides the same way either caller would have wanted.
        store.write().execute(
            "INSERT INTO blobs (id, byte_len, created_us, data)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO NOTHING",
            &vals![id.to_hex(), bytes.len() as i64, to_us(jiff::Timestamp::now()), sealed],
        )?;
        Ok(id)
    }

    fn get(&self, id: BlobId) -> Result<Vec<u8>> {
        let store = self.0;
        let sealed = store
            .read()
            .sealed("SELECT data FROM blobs WHERE id = ?1", &vals![id.to_hex()])?
            .ok_or_else(|| Error::not_found("blob", id))?;
        let geo = Geometry::parse(id, &sealed, sealed.len() as u64)?;
        geo.read_range(store.cipher.as_ref(), id, 0, geo.plain_len, |at, len| {
            Ok(slice(&sealed, at, len))
        })
    }

    fn get_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        let store = self.0;
        // Two round trips: the header, then only the sealed chunks the
        // window touches. The whole point of not fetching the column --
        // scrubbing a video must not download it.
        let geo = self.geometry(id)?;
        geo.read_range(store.cipher.as_ref(), id, offset, len, |at, size| {
            // `substr` is 1-based in both databases, and takes a count
            // rather than an end.
            let row = store.read().query_opt(
                "SELECT substr(data, ?2, ?3) FROM blobs WHERE id = ?1",
                &vals![id.to_hex(), at as i64 + 1, size as i64],
            )?;
            match row {
                Some(r) => r.bytes(0),
                None => Err(Error::not_found("blob", id)),
            }
        })
    }

    fn len_of(&self, id: BlobId) -> Result<u64> {
        // The plaintext length is its own column, so this costs one indexed
        // lookup and reads none of the payload.
        let row = self
            .0
            .read()
            .query_opt("SELECT byte_len FROM blobs WHERE id = ?1", &vals![id.to_hex()])?
            .ok_or_else(|| Error::not_found("blob", id))?;
        row.u64(0)
    }

    fn has(&self, id: BlobId) -> Result<bool> {
        Ok(self
            .0
            .read()
            .query_opt("SELECT 1 FROM blobs WHERE id = ?1", &vals![id.to_hex()])?
            .is_some())
    }

    fn age_of(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        let row = self
            .0
            .read()
            .query_opt("SELECT created_us FROM blobs WHERE id = ?1", &vals![id.to_hex()])?;
        let Some(row) = row else { return Ok(None) };
        // A clock that has gone backwards yields zero rather than an error:
        // callers use this to decide what is *old enough* to delete, so an
        // unreadable age must never read as "ancient".
        let age_us = (to_us(jiff::Timestamp::now()) - row.i64(0)?).max(0);
        Ok(Some(std::time::Duration::from_micros(age_us as u64)))
    }

    fn delete(&self, id: BlobId) -> Result<()> {
        self.0.write().execute("DELETE FROM blobs WHERE id = ?1", &vals![id.to_hex()])?;
        Ok(())
    }

    fn list(&self) -> Result<Vec<BlobId>> {
        let rows = self.0.read().query("SELECT id FROM blobs ORDER BY id", &[])?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(BlobId::parse(&row.text(0)?)?);
        }
        Ok(out)
    }

    fn stats(&self) -> Result<(u64, u64)> {
        let row = self
            .0
            .read()
            .query_opt("SELECT COUNT(*), COALESCE(SUM(length(data)), 0) FROM blobs", &[])?
            .unwrap_or_default();
        Ok((row.u64(0)?, row.u64(1)?))
    }
}

impl TableBlobs<'_> {
    /// A blob's header, without fetching its payload.
    fn geometry(&self, id: BlobId) -> Result<Geometry> {
        let row = self
            .0
            .read()
            .query_opt(
                "SELECT substr(data, 1, ?2), length(data) FROM blobs WHERE id = ?1",
                &vals![id.to_hex(), HEADER_LEN as i64],
            )?
            .ok_or_else(|| Error::not_found("blob", id))?;
        Geometry::parse(id, &row.bytes(0)?, row.u64(1)?)
    }
}

impl SqlStore {
    pub(crate) fn blob_put(&self, bytes: &[u8]) -> Result<BlobId> {
        match &self.media {
            Media::Files(files) => files.put(bytes),
            Media::Table => TableBlobs(self).put(bytes),
        }
    }

    pub(crate) fn blob_get(&self, id: BlobId) -> Result<Vec<u8>> {
        match &self.media {
            Media::Files(files) => files.get(id),
            Media::Table => TableBlobs(self).get(id),
        }
    }

    pub(crate) fn blob_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        match &self.media {
            Media::Files(files) => files.get_range(id, offset, len),
            Media::Table => TableBlobs(self).get_range(id, offset, len),
        }
    }

    pub(crate) fn blob_len(&self, id: BlobId) -> Result<u64> {
        match &self.media {
            Media::Files(files) => files.len_of(id),
            Media::Table => TableBlobs(self).len_of(id),
        }
    }

    pub(crate) fn blob_has(&self, id: BlobId) -> Result<bool> {
        match &self.media {
            Media::Files(files) => Ok(files.has(id)),
            Media::Table => TableBlobs(self).has(id),
        }
    }

    pub(crate) fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        match &self.media {
            Media::Files(files) => Ok(files.age_of(id)),
            Media::Table => TableBlobs(self).age_of(id),
        }
    }

    pub(crate) fn blob_delete(&self, id: BlobId) -> Result<()> {
        match &self.media {
            Media::Files(files) => files.delete(id),
            Media::Table => TableBlobs(self).delete(id),
        }
    }

    pub(crate) fn blob_list(&self) -> Result<Vec<BlobId>> {
        match &self.media {
            Media::Files(files) => files.list(),
            Media::Table => TableBlobs(self).list(),
        }
    }

    /// `(count, total sealed bytes)`.
    pub(crate) fn blob_stats(&self) -> Result<(u64, u64)> {
        match &self.media {
            Media::Files(files) => files.stats(),
            Media::Table => TableBlobs(self).stats(),
        }
    }
}

/// The window `[at, at + len)` of `bytes`, clamped, for an in-memory read.
fn slice(bytes: &[u8], at: u64, len: u64) -> Vec<u8> {
    let start = (at as usize).min(bytes.len());
    let end = start.saturating_add(len as usize).min(bytes.len());
    bytes[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slicing_past_the_end_yields_what_is_there() {
        let bytes = [1u8, 2, 3, 4];
        assert_eq!(slice(&bytes, 1, 2), vec![2, 3]);
        assert_eq!(slice(&bytes, 3, 99), vec![4]);
        assert_eq!(slice(&bytes, 9, 1), Vec::<u8>::new());
    }
}

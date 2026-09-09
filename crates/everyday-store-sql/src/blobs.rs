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

use crate::conn::{Connection, Sql, SqlExt};
use crate::dialect::Dialect;
use crate::{Media, SqlStore, to_us, vals};
use everyday_core::blobstore::{self, Geometry, HEADER_LEN};
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

impl SqlStore {
    pub(crate) fn blob_put(&self, bytes: &[u8]) -> Result<BlobId> {
        match &self.media {
            Media::Files(files) => files.put(bytes),
            Media::Table => {
                if let Some(max) = self.driver.max_blob_bytes()
                    && bytes.len() as u64 > max
                {
                    return Err(Error::Invalid(format!(
                        "this attachment is {} bytes; the {} backend accepts up to {max}",
                        bytes.len(),
                        self.driver.backend_id(),
                    )));
                }
                let id = BlobId::of(bytes);
                // Content-addressed: identical bytes, identical row. Checking
                // first also avoids sealing a 400 MB video to throw it away.
                if self.blob_has(id)? {
                    return Ok(id);
                }
                let sealed = blobstore::seal(self.cipher.as_ref(), id, bytes)?;
                // `DO NOTHING` rather than an error: two threads storing the
                // same photo is a race with one right answer, and it is the
                // row that is already there.
                self.write().execute(
                    "INSERT INTO blobs (id, byte_len, created_us, data)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (id) DO NOTHING",
                    &vals![id.to_hex(), bytes.len() as i64, to_us(jiff::Timestamp::now()), sealed],
                )?;
                Ok(id)
            }
        }
    }

    pub(crate) fn blob_get(&self, id: BlobId) -> Result<Vec<u8>> {
        match &self.media {
            Media::Files(files) => files.get(id),
            Media::Table => {
                let sealed = self
                    .read()
                    .sealed("SELECT data FROM blobs WHERE id = ?1", &vals![id.to_hex()])?
                    .ok_or_else(|| Error::not_found("blob", id))?;
                let geo = Geometry::parse(id, &sealed, sealed.len() as u64)?;
                geo.read_range(self.cipher.as_ref(), id, 0, geo.plain_len, |at, len| {
                    Ok(slice(&sealed, at, len))
                })
            }
        }
    }

    pub(crate) fn blob_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        match &self.media {
            Media::Files(files) => files.get_range(id, offset, len),
            Media::Table => {
                // Two round trips: the header, then only the sealed chunks
                // the window touches. The whole point of not fetching the
                // column -- scrubbing a video must not download it.
                let geo = self.blob_geometry(id)?;
                geo.read_range(self.cipher.as_ref(), id, offset, len, |at, size| {
                    // `substr` is 1-based in both databases, and takes a
                    // count rather than an end.
                    let row = self.read().query_opt(
                        "SELECT substr(data, ?2, ?3) FROM blobs WHERE id = ?1",
                        &vals![id.to_hex(), at as i64 + 1, size as i64],
                    )?;
                    match row {
                        Some(r) => r.bytes(0),
                        None => Err(Error::not_found("blob", id)),
                    }
                })
            }
        }
    }

    pub(crate) fn blob_len(&self, id: BlobId) -> Result<u64> {
        match &self.media {
            Media::Files(files) => files.len_of(id),
            // The plaintext length is its own column, so this costs one
            // indexed lookup and reads none of the payload.
            Media::Table => {
                let row = self
                    .read()
                    .query_opt("SELECT byte_len FROM blobs WHERE id = ?1", &vals![id.to_hex()])?
                    .ok_or_else(|| Error::not_found("blob", id))?;
                row.u64(0)
            }
        }
    }

    pub(crate) fn blob_has(&self, id: BlobId) -> Result<bool> {
        match &self.media {
            Media::Files(files) => Ok(files.has(id)),
            Media::Table => Ok(self
                .read()
                .query_opt("SELECT 1 FROM blobs WHERE id = ?1", &vals![id.to_hex()])?
                .is_some()),
        }
    }

    pub(crate) fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        match &self.media {
            Media::Files(files) => Ok(files.age_of(id)),
            Media::Table => {
                let row = self
                    .read()
                    .query_opt("SELECT created_us FROM blobs WHERE id = ?1", &vals![id.to_hex()])?;
                let Some(row) = row else { return Ok(None) };
                // A clock that has gone backwards yields zero rather than an
                // error: callers use this to decide what is *old enough* to
                // delete, so an unreadable age must never read as "ancient".
                let age_us = (to_us(jiff::Timestamp::now()) - row.i64(0)?).max(0);
                Ok(Some(std::time::Duration::from_micros(age_us as u64)))
            }
        }
    }

    pub(crate) fn blob_delete(&self, id: BlobId) -> Result<()> {
        match &self.media {
            Media::Files(files) => files.delete(id),
            Media::Table => {
                self.write().execute("DELETE FROM blobs WHERE id = ?1", &vals![id.to_hex()])?;
                Ok(())
            }
        }
    }

    pub(crate) fn blob_list(&self) -> Result<Vec<BlobId>> {
        match &self.media {
            Media::Files(files) => files.list(),
            Media::Table => {
                let rows = self.read().query("SELECT id FROM blobs ORDER BY id", &[])?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    out.push(BlobId::parse(&row.text(0)?)?);
                }
                Ok(out)
            }
        }
    }

    /// `(count, total sealed bytes)`.
    pub(crate) fn blob_stats(&self) -> Result<(u64, u64)> {
        match &self.media {
            Media::Files(files) => files.stats(),
            Media::Table => {
                let row = self
                    .read()
                    .query_opt("SELECT COUNT(*), COALESCE(SUM(length(data)), 0) FROM blobs", &[])?
                    .unwrap_or_default();
                Ok((row.u64(0)?, row.u64(1)?))
            }
        }
    }

    /// A blob's header, without fetching its payload.
    fn blob_geometry(&self, id: BlobId) -> Result<Geometry> {
        let row = self
            .read()
            .query_opt(
                "SELECT substr(data, 1, ?2), length(data) FROM blobs WHERE id = ?1",
                &vals![id.to_hex(), HEADER_LEN as i64],
            )?
            .ok_or_else(|| Error::not_found("blob", id))?;
        Geometry::parse(id, &row.bytes(0)?, row.u64(1)?)
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

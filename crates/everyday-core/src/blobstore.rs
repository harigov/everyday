//! Content-addressed, chunk-encrypted storage for attachment payloads.
//!
//! Every backend uses this format rather than inventing one, and a backend
//! whose database is on this machine embeds the [`FileBlobStore`] below as
//! well -- because attachments have requirements the record store does not:
//!
//! * **They are large.** A phone video is three orders of magnitude bigger
//!   than the entry that embeds it. Keeping them out of the database keeps
//!   the database small, fast to open and cheap to back up.
//!
//! * **They must be seekable.** Sealing a 400 MB video as one AEAD message
//!   would force a full decrypt before the first frame could play. Instead
//!   the payload is split into fixed-size chunks, each sealed separately, so
//!   an HTTP range request for the middle of a video decrypts two chunks
//!   rather than 400 MB.
//!
//! # The container format
//!
//! ```text
//!   magic "EVDB" | ver u8 | rsv u8 | chunk u32 | overhead u32 | len u64 | pad
//!   chunk 0 sealed
//!   chunk 1 sealed
//!   ...
//! ```
//!
//! Every chunk but the last has the same sealed size (`chunk + overhead`),
//! which is what makes the offset of chunk *i* computable without an index.
//! Each chunk is sealed with associated data naming the blob, the chunk
//! index and the chunk count, so chunks cannot be reordered, duplicated,
//! truncated or moved between blobs without the tag failing.
//!
//! # The format, and where it is kept, are two questions
//!
//! [`Geometry`] and [`seal`] are the format; [`FileBlobStore`] is one place
//! to put it. A backend whose records live on a server wants the same sealed
//! bytes in a `BYTEA` column instead of in a file -- so that attachments
//! travel with the vault rather than being stranded on whichever laptop
//! pasted them in -- and it gets that by reusing these two rather than by
//! inventing a second container. A blob is then the same sequence of bytes
//! wherever it is stored, which is what makes moving a vault between
//! backends a copy rather than a conversion.
//!
//! The split is also what keeps range reads honest. [`Geometry::read_range`]
//! is handed a closure that fetches sealed bytes from *somewhere* — a
//! `seek`+`read` here, a `substr()` in SQL there — and decides on its own
//! which chunks that range touches. Neither caller can get the arithmetic
//! subtly different from the other, because there is only one copy of it.

use crate::crypto::Cipher;
use crate::error::{Error, Result};
use crate::id::BlobId;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"EVDB";
const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 24;

/// 256 KiB: small enough that a seek wastes little, large enough that
/// per-chunk AEAD overhead is negligible (40 bytes, ~0.015%).
pub const CHUNK_SIZE: u32 = 256 * 1024;

fn chunk_aad(id: BlobId, index: u64, count: u64) -> Vec<u8> {
    format!("everyday.blob.v1:{id}:{index}/{count}").into_bytes()
}

/// How many chunks a payload of `plain_len` bytes occupies.
///
/// `max(1)` because an empty blob is still one (empty) sealed chunk: a zero
/// here would make the count part of the associated data disagree between
/// writer and reader, and an empty attachment would fail to open.
fn chunk_count(plain_len: u64, chunk: u64) -> u64 {
    plain_len.div_ceil(chunk).max(1)
}

/// The 24-byte preamble, given a measured per-chunk overhead.
fn header_bytes(overhead: u32, plain_len: u64) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4] = VERSION;
    header[6..10].copy_from_slice(&CHUNK_SIZE.to_le_bytes());
    header[10..14].copy_from_slice(&overhead.to_le_bytes());
    header[14..22].copy_from_slice(&plain_len.to_le_bytes());
    header
}

/// Seal `bytes` into one complete container: header followed by chunks.
///
/// The whole thing at once, which suits a caller that is going to hand the
/// result to something taking a single value -- a `BYTEA` parameter, a PUT
/// body. [`FileBlobStore`] does not use this: it streams the chunks straight
/// out to disk instead, because a 400 MB video should not exist twice in
/// memory just to be written once.
pub fn seal(cipher: &dyn Cipher, id: BlobId, bytes: &[u8]) -> Result<Vec<u8>> {
    let chunk = CHUNK_SIZE as usize;
    let count = chunk_count(bytes.len() as u64, CHUNK_SIZE as u64);

    // Overhead is a property of the cipher, not of the data, but it is
    // measured rather than assumed so that a future suite with a different
    // tag or nonce size needs no changes here.
    let first = cipher.seal(&chunk_aad(id, 0, count), &bytes[..chunk.min(bytes.len())])?;
    let overhead = (first.len() - chunk.min(bytes.len())) as u32;

    let mut out = Vec::with_capacity(HEADER_LEN + bytes.len() + count as usize * overhead as usize);
    out.extend_from_slice(&header_bytes(overhead, bytes.len() as u64));
    out.extend_from_slice(&first);
    for (i, part) in bytes.chunks(chunk).enumerate().skip(1) {
        out.extend_from_slice(&cipher.seal(&chunk_aad(id, i as u64, count), part)?);
    }
    Ok(out)
}

/// What a container's header says about the payload behind it.
///
/// Parsed once per read and then used to work out which sealed bytes a
/// plaintext range needs. Holds no handle to the storage it came from, which
/// is exactly why it can be shared between a file and a database column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Plaintext bytes per chunk. Every chunk but the last is exactly this.
    pub chunk: u64,
    /// AEAD bytes added to each chunk: nonce plus tag, or zero.
    pub overhead: u64,
    /// Plaintext length of the whole blob.
    pub plain_len: u64,
}

impl Geometry {
    /// Read the header of a container whose sealed length is `sealed_len`.
    ///
    /// # Why the length is checked rather than trusted
    ///
    /// `plain_len` is a 64-bit field in a 24-byte header, so a corrupt or
    /// hostile container can claim to hold exabytes; readers size buffers
    /// from it, and would try to allocate that much before the first byte was
    /// ever decrypted. The geometry is fully determined, so it is simply
    /// checked: a well-formed container is exactly the header, plus the
    /// payload, plus one AEAD overhead per chunk. Anything else is
    /// corruption, and is rejected here rather than turned into an
    /// allocation.
    pub fn parse(id: BlobId, header: &[u8], sealed_len: u64) -> Result<Self> {
        if header.len() < HEADER_LEN {
            return Err(Error::Invalid(format!("blob {id} is truncated")));
        }
        if &header[..4] != MAGIC {
            return Err(Error::Invalid(format!("blob {id} is not an Every Day blob")));
        }
        if header[4] != VERSION {
            return Err(Error::Invalid(format!("blob {id} uses format version {}", header[4])));
        }
        let chunk = u32::from_le_bytes(header[6..10].try_into().unwrap()) as u64;
        let overhead = u32::from_le_bytes(header[10..14].try_into().unwrap()) as u64;
        let plain_len = u64::from_le_bytes(header[14..22].try_into().unwrap());
        if chunk == 0 {
            return Err(Error::Invalid(format!("blob {id} declares a zero chunk size")));
        }

        let count = chunk_count(plain_len, chunk);
        let expected = HEADER_LEN as u128 + plain_len as u128 + count as u128 * overhead as u128;
        if expected != sealed_len as u128 {
            return Err(Error::Invalid(format!(
                "blob {id} declares {plain_len} bytes in {count} chunks \
                 (expecting a {expected}-byte file) but the file is {sealed_len} bytes"
            )));
        }
        Ok(Self { chunk, overhead, plain_len })
    }

    /// Total sealed size of a payload of `plain_len` bytes.
    pub fn sealed_len(&self) -> u64 {
        HEADER_LEN as u64 + self.plain_len + chunk_count(self.plain_len, self.chunk) * self.overhead
    }

    /// Decrypt `len` plaintext bytes from `offset`, fetching only the sealed
    /// chunks that window actually touches.
    ///
    /// `fetch(offset, len)` returns sealed bytes from the container,
    /// container-relative and including the header — a `seek` and a `read`
    /// against a file, a `substring()` against a column. It must return
    /// exactly `len` bytes; anything shorter is reported as truncation.
    ///
    /// A range past the end is clamped, matching how HTTP range requests are
    /// expected to behave.
    pub fn read_range(
        &self,
        cipher: &dyn Cipher,
        id: BlobId,
        offset: u64,
        len: u64,
        mut fetch: impl FnMut(u64, u64) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        if offset >= self.plain_len {
            return Ok(Vec::new());
        }
        let len = len.min(self.plain_len - offset);
        if len == 0 {
            return Ok(Vec::new());
        }
        let count = chunk_count(self.plain_len, self.chunk);
        let first = offset / self.chunk;
        let last = ((offset + len - 1) / self.chunk).min(count - 1);

        let mut out = Vec::with_capacity(len as usize);
        for i in first..=last {
            let plain_start = i * self.chunk;
            let plain_size = self.chunk.min(self.plain_len.saturating_sub(plain_start));
            let sealed_size = plain_size + self.overhead;
            let at = HEADER_LEN as u64 + i * (self.chunk + self.overhead);

            let sealed = fetch(at, sealed_size)?;
            if sealed.len() as u64 != sealed_size {
                return Err(Error::Invalid(format!("blob {id} is truncated at chunk {i}")));
            }
            let plain = cipher.open(&chunk_aad(id, i, count), &sealed)?;

            // Trim the chunk down to the requested window.
            let from = offset.saturating_sub(plain_start) as usize;
            let to = (((offset + len) - plain_start) as usize).min(plain.len());
            if from < plain.len() {
                out.extend_from_slice(&plain[from..to]);
            }
        }
        Ok(out)
    }
}

/// A directory of sealed, content-addressed blobs.
pub struct FileBlobStore {
    root: PathBuf,
    cipher: Arc<dyn Cipher>,
}

impl std::fmt::Debug for FileBlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileBlobStore").field("root", &self.root).finish()
    }
}

impl FileBlobStore {
    pub fn open(root: impl Into<PathBuf>, cipher: Arc<dyn Cipher>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
        Ok(Self { root, cipher })
    }

    /// Blobs are fanned out over 256 subdirectories by their first byte, so
    /// no single directory ends up with 50 000 entries in it.
    fn path_for(&self, id: BlobId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join(&hex[..2]).join(format!("{hex}.blob"))
    }

    pub fn has(&self, id: BlobId) -> bool {
        self.path_for(id).is_file()
    }

    /// Seal `bytes` and write them. Returns the content address.
    ///
    /// Writing is atomic: the sealed file is built under a temporary name and
    /// renamed into place, so a crash mid-write cannot leave a blob that
    /// exists but fails to decrypt.
    pub fn put(&self, bytes: &[u8]) -> Result<BlobId> {
        let id = BlobId::of(bytes);
        let path = self.path_for(id);
        if path.is_file() {
            return Ok(id); // content-addressed: identical bytes, identical file
        }
        let dir = path.parent().expect("blob path always has a parent");
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;

        let chunk = CHUNK_SIZE as usize;
        let count = chunk_count(bytes.len() as u64, CHUNK_SIZE as u64);

        // Overhead is a property of the cipher, not of the data, but we
        // measure it rather than assume it so that a future suite with a
        // different tag or nonce size needs no changes here.
        let probe = self.cipher.seal(&chunk_aad(id, 0, count), &bytes[..chunk.min(bytes.len())])?;
        let overhead = (probe.len() - chunk.min(bytes.len())) as u32;

        // The temp name carries a per-writer tag, not just the blob id.
        // Deriving it from the id alone meant two threads sealing the *same*
        // bytes -- the same photo attached twice, an import running beside an
        // editor -- shared one scratch file: each truncated the other's
        // partial write, and whichever renamed second failed with `NotFound`
        // because the first had already moved it away.
        let tmp = path.with_extension(format!("{}.tmp", crate::fsutil::unique_tag()));
        {
            let mut f = File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
            f.write_all(&header_bytes(overhead, bytes.len() as u64))
                .map_err(|e| Error::io(&tmp, e))?;

            if bytes.is_empty() {
                f.write_all(&probe).map_err(|e| Error::io(&tmp, e))?;
            } else {
                for (i, part) in bytes.chunks(chunk).enumerate() {
                    let sealed = if i == 0 {
                        probe.clone()
                    } else {
                        self.cipher.seal(&chunk_aad(id, i as u64, count), part)?
                    };
                    f.write_all(&sealed).map_err(|e| Error::io(&tmp, e))?;
                }
            }
            f.sync_all().map_err(|e| Error::io(&tmp, e))?;
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::io(&path, e));
        }
        // The bytes were fsynced above; this is the rename itself reaching
        // the platter, so the blob cannot vanish out from under an entry that
        // already references it.
        crate::fsutil::sync_dir(dir);
        Ok(id)
    }

    pub fn get(&self, id: BlobId) -> Result<Vec<u8>> {
        let (geo, file) = self.read_header(id)?;
        read_from(&geo, self.cipher.as_ref(), id, file, 0, geo.plain_len)
    }

    /// Read `len` plaintext bytes starting at `offset`, decrypting only the
    /// chunks the range actually touches.
    ///
    /// A range past the end of the blob is clamped, matching how HTTP range
    /// requests are expected to behave.
    pub fn get_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        let (geo, file) = self.read_header(id)?;
        read_from(&geo, self.cipher.as_ref(), id, file, offset, len)
    }

    /// Plaintext length, without decrypting anything.
    pub fn len_of(&self, id: BlobId) -> Result<u64> {
        Ok(self.read_header(id)?.0.plain_len)
    }

    /// How long ago this blob was written, for garbage collection.
    ///
    /// `None` for a blob that is not there, or whose timestamp the platform
    /// will not report. A clock that has gone backwards since the write
    /// yields `Duration::ZERO` rather than an error -- callers use this to
    /// decide what is *old enough* to delete, so an unreadable age must
    /// never read as "ancient".
    pub fn age_of(&self, id: BlobId) -> Option<std::time::Duration> {
        let modified = std::fs::metadata(self.path_for(id)).and_then(|m| m.modified()).ok()?;
        Some(modified.elapsed().unwrap_or(std::time::Duration::ZERO))
    }

    pub fn delete(&self, id: BlobId) -> Result<()> {
        let path = self.path_for(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            // Deleting something that is already gone is the desired state.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    pub fn list(&self) -> Result<Vec<BlobId>> {
        let mut out = Vec::new();
        let dirs = match std::fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(Error::io(&self.root, e)),
        };
        for shard in dirs.flatten() {
            if !shard.path().is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(shard.path()) else { continue };
            for f in files.flatten() {
                let name = f.file_name();
                let name = name.to_string_lossy();
                if let Some(hex) = name.strip_suffix(".blob")
                    && let Ok(id) = BlobId::parse(hex)
                {
                    out.push(id);
                }
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// `(count, total sealed bytes on disk)`.
    pub fn stats(&self) -> Result<(u64, u64)> {
        let mut count = 0;
        let mut bytes = 0;
        for id in self.list()? {
            count += 1;
            if let Ok(m) = std::fs::metadata(self.path_for(id)) {
                bytes += m.len();
            }
        }
        Ok((count, bytes))
    }

    // ---- internals ------------------------------------------------------

    /// Parse a blob's header, handing back the geometry and the open file.
    fn read_header(&self, id: BlobId) -> Result<(Geometry, File)> {
        let path = self.path_for(id);
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::not_found("blob", id));
            }
            Err(e) => return Err(Error::io(&path, e)),
        };
        let mut buf = [0u8; HEADER_LEN];
        f.read_exact(&mut buf).map_err(|_| Error::Invalid(format!("blob {id} is truncated")))?;
        let on_disk = f.metadata().map_err(|e| Error::io(&path, e))?.len();
        Ok((Geometry::parse(id, &buf, on_disk)?, f))
    }
}

/// Decrypt a window of an open container file.
///
/// The whole of the seeking and chunk arithmetic lives in
/// [`Geometry::read_range`]; this supplies it with the one thing that is
/// specific to a file, which is how to get sealed bytes out of one.
fn read_from(
    geo: &Geometry,
    cipher: &dyn Cipher,
    id: BlobId,
    mut file: File,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>> {
    geo.read_range(cipher, id, offset, len, move |at, size| {
        file.seek(SeekFrom::Start(at)).map_err(Error::RawIo)?;
        let mut sealed = vec![0u8; size as usize];
        match file.read_exact(&mut sealed) {
            Ok(()) => Ok(sealed),
            // A short read is truncation; `read_range` says so by name.
            Err(_) => Ok(Vec::new()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{AeadCipher, NullCipher, SecretKey};
    use std::path::Path;

    fn store(dir: &Path, encrypted: bool) -> FileBlobStore {
        let cipher: Arc<dyn Cipher> = if encrypted {
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([3u8; 32])))
        } else {
            Arc::new(NullCipher)
        };
        FileBlobStore::open(dir, cipher).unwrap()
    }

    fn pseudorandom(n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        let mut x: u32 = 0x1234_5678;
        for _ in 0..n {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            v.push((x >> 24) as u8);
        }
        v
    }

    #[test]
    fn round_trips_small_and_multi_chunk_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        for size in [
            0usize,
            1,
            1024,
            CHUNK_SIZE as usize - 1,
            CHUNK_SIZE as usize,
            CHUNK_SIZE as usize + 1,
            CHUNK_SIZE as usize * 3 + 77,
        ] {
            let data = pseudorandom(size);
            let id = s.put(&data).unwrap();
            assert_eq!(s.get(id).unwrap(), data, "round trip failed at size {size}");
            assert_eq!(s.len_of(id).unwrap(), size as u64);
        }
    }

    #[test]
    fn works_with_a_zero_overhead_cipher_too() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), false);
        let data = pseudorandom(CHUNK_SIZE as usize * 2 + 5);
        let id = s.put(&data).unwrap();
        assert_eq!(s.get(id).unwrap(), data);
        assert_eq!(s.get_range(id, 300_000, 1000).unwrap(), data[300_000..301_000]);
    }

    #[test]
    fn range_reads_return_exactly_the_requested_window() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let data = pseudorandom(CHUNK_SIZE as usize * 3 + 1234);
        let id = s.put(&data).unwrap();

        for (off, len) in [
            (0u64, 10u64),
            (1, 1),
            (CHUNK_SIZE as u64 - 5, 10), // straddles a chunk boundary
            (CHUNK_SIZE as u64, 100),    // starts exactly on one
            (CHUNK_SIZE as u64 * 2 + 7, 200_000), // spans two chunks
            (data.len() as u64 - 1, 1),  // final byte
        ] {
            let got = s.get_range(id, off, len).unwrap();
            let want = &data[off as usize..(off + len) as usize];
            assert_eq!(got, want, "range ({off}, {len}) mismatched");
        }
    }

    #[test]
    fn a_range_past_the_end_is_clamped_rather_than_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let data = pseudorandom(100);
        let id = s.put(&data).unwrap();

        assert_eq!(s.get_range(id, 90, 1000).unwrap(), data[90..]);
        assert!(s.get_range(id, 100, 10).unwrap().is_empty());
        assert!(s.get_range(id, 10_000, 10).unwrap().is_empty());
        assert!(s.get_range(id, 0, 0).unwrap().is_empty());
    }

    #[test]
    fn identical_bytes_deduplicate_to_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let a = s.put(b"same bytes").unwrap();
        let b = s.put(b"same bytes").unwrap();
        assert_eq!(a, b);
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn missing_blobs_report_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let ghost = BlobId::of(b"never written");
        assert!(!s.has(ghost));
        assert_eq!(s.get(ghost).unwrap_err().code(), "not_found");
        assert_eq!(s.len_of(ghost).unwrap_err().code(), "not_found");
        // Deleting a blob that is not there is a no-op, not an error.
        s.delete(ghost).unwrap();
    }

    #[test]
    fn delete_removes_the_blob() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(b"transient").unwrap();
        assert!(s.has(id));
        s.delete(id).unwrap();
        assert!(!s.has(id));
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn on_disk_bytes_are_not_the_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(b"the diary of a nobody").unwrap();
        let raw = std::fs::read(s.path_for(id)).unwrap();
        assert!(!raw.windows(21).any(|w| w == b"the diary of a nobody"));
    }

    #[test]
    fn a_tampered_chunk_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(&pseudorandom(1000)).unwrap();

        let path = s.path_for(id);
        let mut raw = std::fs::read(&path).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xff;
        std::fs::write(&path, raw).unwrap();

        assert_eq!(s.get(id).unwrap_err().code(), "decrypt_failed");
    }

    #[test]
    fn chunks_cannot_be_swapped_between_blobs() {
        // The per-chunk AAD names the blob, so grafting a chunk from another
        // blob must fail even though both were sealed with the same key.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let a = s.put(&pseudorandom(1000)).unwrap();
        let b = s.put(&pseudorandom(1001)).unwrap();

        let mut raw_a = std::fs::read(s.path_for(a)).unwrap();
        let raw_b = std::fs::read(s.path_for(b)).unwrap();
        let n = raw_a.len() - HEADER_LEN;
        raw_a[HEADER_LEN..].copy_from_slice(&raw_b[HEADER_LEN..HEADER_LEN + n]);
        std::fs::write(s.path_for(a), raw_a).unwrap();

        assert_eq!(s.get(a).unwrap_err().code(), "decrypt_failed");
    }

    #[test]
    fn a_truncated_file_is_reported_rather_than_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(&pseudorandom(CHUNK_SIZE as usize + 10)).unwrap();

        let path = s.path_for(id);
        let raw = std::fs::read(&path).unwrap();
        std::fs::write(&path, &raw[..raw.len() / 2]).unwrap();

        assert_eq!(s.get(id).unwrap_err().code(), "invalid");
    }

    #[test]
    fn a_header_declaring_an_impossible_length_is_rejected_without_allocating() {
        // Regression: `plain_len` used to be trusted, so a 24-byte file could
        // make a reader ask the allocator for four exabytes.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(b"small").unwrap();

        let path = s.path_for(id);
        let mut raw = std::fs::read(&path).unwrap();
        raw[14..22].copy_from_slice(&(u64::MAX / 4).to_le_bytes());
        std::fs::write(&path, raw).unwrap();

        // Must return an error promptly rather than trying to reserve it.
        assert_eq!(s.get(id).unwrap_err().code(), "invalid");
        assert_eq!(s.len_of(id).unwrap_err().code(), "invalid");
        assert_eq!(s.get_range(id, 0, 16).unwrap_err().code(), "invalid");
    }

    #[test]
    fn a_header_declaring_an_absurd_overhead_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(b"small").unwrap();

        let path = s.path_for(id);
        let mut raw = std::fs::read(&path).unwrap();
        raw[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, raw).unwrap();

        assert_eq!(s.get(id).unwrap_err().code(), "invalid");
    }

    #[test]
    fn appending_junk_to_a_blob_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(b"small").unwrap();

        let path = s.path_for(id);
        let mut raw = std::fs::read(&path).unwrap();
        raw.extend_from_slice(&[0u8; 64]);
        std::fs::write(&path, raw).unwrap();

        assert_eq!(s.get(id).unwrap_err().code(), "invalid");
    }

    #[test]
    fn a_foreign_file_in_the_blob_directory_is_rejected_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = BlobId::of(b"pretend");
        let path = s.path_for(id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not a blob file at all").unwrap();

        assert_eq!(s.get(id).unwrap_err().code(), "invalid");
    }

    #[test]
    fn stats_count_files_and_sealed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        s.put(b"one").unwrap();
        s.put(&pseudorandom(5000)).unwrap();
        let (count, bytes) = s.stats().unwrap();
        assert_eq!(count, 2);
        assert!(bytes > 5000, "sealed size should exceed plaintext size");
    }

    #[test]
    fn list_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let id = s.put(b"real").unwrap();
        std::fs::write(dir.path().join("README.txt"), b"hello").unwrap();
        std::fs::write(s.path_for(id).with_extension("tmp"), b"leftover").unwrap();
        assert_eq!(s.list().unwrap(), [id]);
    }

    #[test]
    fn concurrent_puts_of_identical_bytes_all_succeed() {
        // Regression: the temp file was named from the blob id alone, so
        // every writer of the same bytes shared it. Three of four concurrent
        // puts failed with `NotFound` on the rename, which reached the user
        // as an attachment that would not save.
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(store(dir.path(), true));
        let data = Arc::new(pseudorandom(CHUNK_SIZE as usize * 2 + 11));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (s, data) = (s.clone(), data.clone());
                std::thread::spawn(move || s.put(&data))
            })
            .collect();

        let ids: Vec<BlobId> = handles
            .into_iter()
            .map(|h| h.join().unwrap().expect("every concurrent put should succeed"))
            .collect();

        assert!(ids.windows(2).all(|w| w[0] == w[1]), "content addressing should agree");
        assert_eq!(s.get(ids[0]).unwrap(), *data);
        assert_eq!(s.list().unwrap(), [ids[0]], "no scratch files left behind");
    }

    #[test]
    fn a_blob_written_under_one_key_does_not_open_under_another() {
        let dir = tempfile::tempdir().unwrap();
        let id = store(dir.path(), true).put(b"private").unwrap();

        let other = FileBlobStore::open(
            dir.path(),
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([9u8; 32]))),
        )
        .unwrap();
        assert_eq!(other.get(id).unwrap_err().code(), "decrypt_failed");
    }
}

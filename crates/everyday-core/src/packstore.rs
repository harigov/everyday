//! Storage for raw messages, sealed one at a time, without one file per
//! message.
//!
//! A hundred thousand messages is a hundred thousand `fsync`s and a hundred
//! thousand inodes if each becomes its own entry in
//! [`FileBlobStore`](crate::blobstore::FileBlobStore) -- fine for a photo
//! pasted into an entry once in a while, ruinous for what mail sync does on
//! its first pass through an inbox. [`PackStore`] is the other shape:
//! messages sealed individually, so reading one never decrypts its
//! neighbours, but *flushed* together, so writing a thousand of them costs
//! one `fsync` rather than a thousand.
//!
//! # Why this exists ahead of the mail domain
//!
//! `docs/plans/mail.md` calls this "phase 0 ... the part of this plan that
//! is not about mail at all", and lays out the design this module follows:
//! append-only pack files, each message sealed on its own, flushed once per
//! batch, with a compaction pass that rewrites a pack once a third of it is
//! dead. Nothing here names a message, an account record or a mailbox --
//! those arrive with the domain in a later phase -- but the shape a hundred
//! thousand raw messages need does not depend on any of that, so it is built
//! and proven first.
//!
//! # One trait, two backings
//!
//! The same split [`blobstore`](crate::blobstore) makes for attachments,
//! for the same reason. [`FilePackStore`] below is what a vault whose
//! database is on this machine wants: append-only files beside wherever the
//! blob store keeps its media, each one a sealed log the filesystem already
//! buffers and syncs. A vault whose database is a server has no local disk
//! to put a file on, so `everyday-store-sql` gives the same trait a
//! `mail_packs`-table answer instead -- see that crate's `packs` module --
//! and the two share nothing but the trait and the cipher underneath them.
//!
//! # The frame format
//!
//! One pack file is a sequence of independently sealed frames:
//!
//! ```text
//!   len (u32 LE) | sealed bytes (nonce || ciphertext || tag)
//!   len (u32 LE) | sealed bytes
//!   ...
//! ```
//!
//! `len` is the sealed length, in the clear -- the same trade every framing
//! format in this codebase makes, and it reveals nothing a directory listing
//! of individually-sealed attachments would not. Each frame is sealed with
//! associated data naming the pack and the frame's own offset (see
//! [`pack_aad`]), which is what makes [`FilePackStore::read`] a seek and an
//! open rather than a scan from the start, and what stops a frame moved or
//! duplicated within a pack -- or grafted from a different one -- decrypting
//! as though nothing had changed.
//!
//! # Recovering from a crash mid-write
//!
//! A batch is flushed once, at the end, after every frame in it has been
//! written -- so the only way a pack can hold a torn frame is a crash
//! between two of those writes. [`FilePackStore`] does not trust the
//! file's length at face value: before it appends, it walks the pack's
//! frames from the start, stops at the first one that does not fully fit in
//! what is on disk, and truncates away everything after that point. Every
//! frame before it was the last byte of a batch that finished and was
//! flushed, so it is kept; anything after it was never durable and is
//! discarded, the same way a half-written WAL record is discarded by any
//! database that replays one.

use crate::crypto::Cipher;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::id::PackId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A length-prefixed frame's length field, in bytes.
const FRAME_PREFIX_LEN: u64 = 4;

/// A pack stops taking new messages once it reaches this size and a fresh
/// one is started instead.
///
/// Bounded packs are what makes compaction cheap: rewriting a pack means
/// decrypting and re-sealing everything still live in it, and a vault whose
/// packs could grow forever would make that cost unbounded too. 64 MiB holds
/// tens of thousands of ordinary messages -- comfortably more than one
/// account's IMAP fetch produces in one sync batch -- while staying small
/// enough that a full rewrite is milliseconds of work, not seconds.
pub const PACK_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

/// Where one message lives: which pack, and where in it.
///
/// This is the whole address -- `read` needs nothing else to find and open
/// the bytes it names. `account` is carried on the reference itself, rather
/// than passed alongside it the way it is to [`PackStore::append_batch`],
/// because a reference has to be able to answer "where am I" entirely on its
/// own: it is what `messages.pack_id`, `pack_offset` and `pack_len` will
/// hold once the mail domain exists to have a `messages` table at all (see
/// the schema section of `docs/plans/mail.md`), long after the
/// [`PackStore`] that minted it is out of scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackRef {
    /// The account this pack belongs to, exactly as it was passed to
    /// [`PackStore::append_batch`].
    pub account: String,
    /// Which pack -- which file, on a file-backed store; which row's
    /// identity, on a table-backed one.
    pub pack: PackId,
    /// Byte offset of the sealed message, *after* any framing prefix a
    /// backend uses. A table-backed store, which frames nothing, leaves
    /// this `0`.
    pub offset: u64,
    /// Sealed length in bytes.
    pub len: u32,
}

/// Storage for raw messages, sealed on their own but flushed together.
///
/// See the module docs for the shape this exists to give: one `fsync` per
/// batch rather than one per message, and a read that touches only the
/// message asked for.
pub trait PackStore: Send + Sync {
    /// Seal and append every message in `messages`, in order, to `account`'s
    /// current pack, flushing once when the whole batch has been written.
    ///
    /// An empty `messages` touches no storage at all and returns an empty
    /// vector -- there is no batch to flush.
    fn append_batch(&self, account: &str, messages: &[&[u8]]) -> Result<Vec<PackRef>>;

    /// The one message `r` names, decrypted, without touching anything
    /// sealed beside it.
    fn read(&self, r: &PackRef) -> Result<Vec<u8>>;

    /// Mark every message in `refs` as no longer worth keeping.
    ///
    /// Does not reclaim the space itself -- a pack is an append-only log, not
    /// something a single dead frame can be cut out of in place -- that is
    /// what [`PackStore::compact`] is for. Marking a message dead twice, or
    /// one this store has already forgotten, is not an error.
    fn mark_dead(&self, refs: &[PackRef]) -> Result<()>;

    /// Rewrite every pack of `account`'s that is at least a third dead,
    /// dropping what [`PackStore::mark_dead`] marked and keeping everything
    /// else.
    ///
    /// The new pack is written and made durable in full before the old one
    /// is removed, so a crash mid-compaction leaves either the untouched
    /// original pack or the finished replacement -- never a gap where
    /// neither is readable. Returns the old reference and the new one for
    /// every message that moved, in no particular order, so a caller can
    /// update whatever else was pointing at the old address (a `messages`
    /// row's `pack_id`, `pack_offset` and `pack_len`, once that table
    /// exists). A pack under the third-dead threshold is left exactly where
    /// it is and contributes nothing to the result.
    fn compact(&self, account: &str) -> Result<Vec<(PackRef, PackRef)>>;
}

/// Associated data binding a sealed frame to the one pack and offset it was
/// written at.
///
/// Binding the offset, not just the pack, is what makes a frame's identity
/// track where it actually lives: two frames written to the same pack seal
/// under different associated data even though they share a `PackId`, so
/// one cannot be copied over the other -- or read at the wrong offset after
/// some future bug got the arithmetic wrong -- and have it open.
pub fn pack_aad(pack: PackId, offset: u64) -> Vec<u8> {
    format!("everyday.mailpack.v1:{pack}:{offset}").into_bytes()
}

/// Append-only pack files beside wherever a local vault keeps its media.
///
/// One subdirectory per account, named by a hash of the account string
/// rather than the string itself -- the same reason [`FileBlobStore`] fans
/// blobs out by content hash rather than by anything meaningful: an account
/// id is not secret, but there is no reason for a directory listing to say
/// anything at all, and hashing costs nothing an opaque UUID would not
/// already have cost. Within that directory, packs are named by their
/// [`PackId`] and rotate once they reach [`PACK_ROTATE_BYTES`]; see the
/// module docs for the frame format inside one.
///
/// [`FileBlobStore`]: crate::blobstore::FileBlobStore
pub struct FilePackStore {
    root: PathBuf,
    cipher: Arc<dyn Cipher>,
    /// Serialises every method against every other, on this store. A single
    /// coarse lock rather than one per account: the vault this exists for
    /// already promises exactly one writer at a time -- see "the existing
    /// write claim" in `docs/plans/mail.md`'s risk table -- so this is a
    /// safety net against two tasks in the *same* process racing each
    /// other, not a throughput design.
    lock: Mutex<()>,
}

impl std::fmt::Debug for FilePackStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePackStore").field("root", &self.root).finish()
    }
}

impl FilePackStore {
    /// Packs rooted at `root`, sealed with `cipher`. `root` is created if it
    /// does not exist, exactly as [`FileBlobStore::open`] creates its own
    /// directory.
    ///
    /// [`FileBlobStore::open`]: crate::blobstore::FileBlobStore::open
    pub fn open(root: impl Into<PathBuf>, cipher: Arc<dyn Cipher>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
        Ok(Self { root, cipher, lock: Mutex::new(()) })
    }

    fn account_dir(&self, account: &str) -> PathBuf {
        self.root.join(blake3::hash(account.as_bytes()).to_hex().to_string())
    }

    /// Every pack that exists for an account, oldest first. `PackId`s are
    /// UUIDv7, so sorting the ids sorts by creation time with no extra work.
    fn list_packs(dir: &Path) -> Result<Vec<PackId>> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(Error::io(dir, e)),
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".pack")
                && let Ok(id) = PackId::parse(stem)
            {
                out.push(id);
            }
        }
        out.sort();
        Ok(out)
    }

    /// The pack an [`FilePackStore::append_batch`] call should write to:
    /// the newest existing one, if it has room, otherwise a fresh one.
    ///
    /// Recovers the newest pack's valid length on the way, which is what
    /// turns "reopen after a crash" into "keep appending after a crash":
    /// see [`recover_valid_len`].
    fn current_pack(&self, dir: &Path) -> Result<(PackId, PathBuf, u64)> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        if let Some(pack) = Self::list_packs(dir)?.into_iter().next_back() {
            let path = dir.join(format!("{pack}.pack"));
            let valid_len = recover_valid_len(&path)?;
            if valid_len < PACK_ROTATE_BYTES {
                return Ok((pack, path, valid_len));
            }
        }
        let pack = PackId::new();
        let path = dir.join(format!("{pack}.pack"));
        File::create(&path).map_err(|e| Error::io(&path, e))?;
        Ok((pack, path, 0))
    }
}

impl PackStore for FilePackStore {
    fn append_batch(&self, account: &str, messages: &[&[u8]]) -> Result<Vec<PackRef>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        let (pack, path, mut offset) = self.current_pack(&dir)?;
        let mut file =
            OpenOptions::new().append(true).open(&path).map_err(|e| Error::io(&path, e))?;

        let mut refs = Vec::with_capacity(messages.len());
        for msg in messages {
            let frame_offset = offset + FRAME_PREFIX_LEN;
            let sealed = self.cipher.seal(&pack_aad(pack, frame_offset), msg)?;
            let len = sealed.len() as u32;
            file.write_all(&(len).to_le_bytes()).map_err(|e| Error::io(&path, e))?;
            file.write_all(&sealed).map_err(|e| Error::io(&path, e))?;
            offset = frame_offset + len as u64;
            refs.push(PackRef { account: account.to_string(), pack, offset: frame_offset, len });
        }
        // One flush for the whole batch, which is the entire point: a
        // thousand messages cost one `fsync`, not a thousand.
        file.sync_all().map_err(|e| Error::io(&path, e))?;
        // Only needed the first time a pack is created, but cheap and
        // idempotent to call regardless -- see `fsutil::sync_dir`.
        fsutil::sync_dir(&dir);
        Ok(refs)
    }

    fn read(&self, r: &PackRef) -> Result<Vec<u8>> {
        let path = self.account_dir(&r.account).join(format!("{}.pack", r.pack));
        let mut file = File::open(&path).map_err(|e| Error::io(&path, e))?;
        file.seek(SeekFrom::Start(r.offset)).map_err(|e| Error::io(&path, e))?;
        let mut sealed = vec![0u8; r.len as usize];
        file.read_exact(&mut sealed).map_err(|_| {
            Error::Invalid(format!("pack {} is truncated at offset {}", r.pack, r.offset))
        })?;
        self.cipher.open(&pack_aad(r.pack, r.offset), &sealed)
    }

    fn mark_dead(&self, refs: &[PackRef]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut by_pack: BTreeMap<(String, PackId), Vec<u64>> = BTreeMap::new();
        for r in refs {
            by_pack.entry((r.account.clone(), r.pack)).or_default().push(r.offset);
        }
        for ((account, pack), offsets) in by_pack {
            let dir = self.account_dir(&account);
            let path = dir.join(format!("{pack}.dead"));
            let mut text = String::new();
            for offset in offsets {
                text.push_str(&offset.to_string());
                text.push('\n');
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| Error::io(&path, e))?;
            file.write_all(text.as_bytes()).map_err(|e| Error::io(&path, e))?;
            file.sync_all().map_err(|e| Error::io(&path, e))?;
        }
        Ok(())
    }

    fn compact(&self, account: &str) -> Result<Vec<(PackRef, PackRef)>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        let mut remap = Vec::new();

        for pack in Self::list_packs(&dir)? {
            let path = dir.join(format!("{pack}.pack"));
            let dead_path = dir.join(format!("{pack}.dead"));
            let valid_len = recover_valid_len(&path)?;
            let dead = read_dead_offsets(&dead_path)?;

            let mut file = File::open(&path).map_err(|e| Error::io(&path, e))?;
            let mut pos = 0u64;
            let mut total = 0u64;
            let mut live: Vec<(u64, u32)> = Vec::new();
            while pos + FRAME_PREFIX_LEN <= valid_len {
                file.seek(SeekFrom::Start(pos)).map_err(|e| Error::io(&path, e))?;
                let mut len_buf = [0u8; FRAME_PREFIX_LEN as usize];
                file.read_exact(&mut len_buf).map_err(|e| Error::io(&path, e))?;
                let len = u32::from_le_bytes(len_buf);
                let frame_offset = pos + FRAME_PREFIX_LEN;
                total += 1;
                if !dead.contains(&frame_offset) {
                    live.push((frame_offset, len));
                }
                pos = frame_offset + len as u64;
            }
            if total == 0 || (total - live.len() as u64) * 3 < total {
                // Fewer than a third dead (or nothing in the pack at all):
                // leave it exactly where it is.
                continue;
            }

            let new_pack = PackId::new();
            let mut rewritten = Vec::new();
            let mut new_offset = 0u64;
            let mut this_pack_remap = Vec::with_capacity(live.len());
            for (offset, len) in live {
                file.seek(SeekFrom::Start(offset)).map_err(|e| Error::io(&path, e))?;
                let mut sealed = vec![0u8; len as usize];
                file.read_exact(&mut sealed).map_err(|e| Error::io(&path, e))?;
                let plain = self.cipher.open(&pack_aad(pack, offset), &sealed)?;

                let new_frame_offset = new_offset + FRAME_PREFIX_LEN;
                let resealed = self.cipher.seal(&pack_aad(new_pack, new_frame_offset), &plain)?;
                let new_len = resealed.len() as u32;
                rewritten.extend_from_slice(&new_len.to_le_bytes());
                rewritten.extend_from_slice(&resealed);
                new_offset = new_frame_offset + new_len as u64;

                this_pack_remap.push((
                    PackRef { account: account.to_string(), pack, offset, len },
                    PackRef {
                        account: account.to_string(),
                        pack: new_pack,
                        offset: new_frame_offset,
                        len: new_len,
                    },
                ));
            }

            // The new pack is written and fsynced -- by `write_atomic` -- in
            // full before the old one is touched at all. A crash here still
            // leaves a reader with a complete, correct pack to find: either
            // this one has not appeared yet and the old one is exactly as it
            // was, or it has and every ref in `this_pack_remap` already
            // resolves against it.
            let new_path = dir.join(format!("{new_pack}.pack"));
            fsutil::write_atomic(&new_path, &rewritten, &fsutil::unique_tag())?;

            drop(file);
            std::fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
            let _ = std::fs::remove_file(&dead_path);
            fsutil::sync_dir(&dir);

            remap.extend(this_pack_remap);
        }
        Ok(remap)
    }
}

/// Walk `path`'s frames from the start, and truncate away anything after the
/// last one that is fully there.
///
/// Returns the number of bytes that are valid -- which, after this call, is
/// also the file's length. A file that does not exist yet has zero valid
/// bytes and nothing to truncate.
///
/// This is an `O(frames already in the pack)` scan, run on every
/// [`FilePackStore::append_batch`] call against the pack being appended to.
/// That is deliberately not optimised further here: packs are bounded by
/// [`PACK_ROTATE_BYTES`], so the scan is bounded too, and it reads only the
/// four-byte length prefix of each frame rather than any sealed bytes. If
/// profiling ever says otherwise, the fix is a cached "known-good length"
/// rather than a change to what this function promises.
fn recover_valid_len(path: &Path) -> Result<u64> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::io(path, e)),
    };
    let total = file.metadata().map_err(|e| Error::io(path, e))?.len();

    let mut pos = 0u64;
    loop {
        if pos + FRAME_PREFIX_LEN > total {
            break;
        }
        file.seek(SeekFrom::Start(pos)).map_err(|e| Error::io(path, e))?;
        let mut len_buf = [0u8; FRAME_PREFIX_LEN as usize];
        if file.read_exact(&mut len_buf).is_err() {
            break;
        }
        let len = u32::from_le_bytes(len_buf) as u64;
        let frame_end = pos + FRAME_PREFIX_LEN + len;
        if frame_end > total {
            break;
        }
        pos = frame_end;
    }

    if pos != total {
        drop(file);
        let trimmed = OpenOptions::new().write(true).open(path).map_err(|e| Error::io(path, e))?;
        trimmed.set_len(pos).map_err(|e| Error::io(path, e))?;
        trimmed.sync_all().map_err(|e| Error::io(path, e))?;
    }
    Ok(pos)
}

/// The frame offsets `mark_dead` recorded for one pack, or an empty set if
/// nothing has ever been marked dead in it.
///
/// One offset per line, in the clear: it is a byte position, not a message,
/// and reveals nothing sealing would protect.
fn read_dead_offsets(path: &Path) -> Result<BTreeSet<u64>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(Error::io(path, e)),
    };
    Ok(text.lines().filter_map(|l| l.parse().ok()).collect())
}

/// A shared conformance suite, the same shape
/// [`store::conformance`](crate::store::conformance) gives every
/// [`JournalStore`](crate::store::JournalStore) implementation -- kept here
/// rather than folded into that module because [`PackStore`] answers to no
/// domain and is reached directly, not through a `JournalStore` accessor.
///
/// Covers only what every implementation can be asked to prove through the
/// trait alone: a round trip, a batch large enough to matter, and
/// compaction's two promises -- live messages survive with new addresses,
/// and a pack under the dead threshold is left untouched. What it does not
/// cover is tampering and crash recovery, because proving either means
/// reaching *underneath* the trait at bytes a table-backed implementation
/// does not expose the same way a file does -- [`FilePackStore`]'s own tests
/// cover those for the file backing, and a table-backed one is expected to
/// write its own in the same spirit, reaching into its rows the way
/// [`FilePackStore`]'s reach into its files.
///
/// The store must hold no packs for `account` on entry, and is left with
/// none on success.
#[cfg(any(test, feature = "testing"))]
pub fn run_pack_store_suite(store: &dyn PackStore, account: &str) {
    eprintln!("--- pack store conformance suite ---");

    let msgs: Vec<Vec<u8>> =
        vec![b"hello".to_vec(), b"world, a little longer".to_vec(), Vec::new()];
    let borrowed: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
    let refs = store.append_batch(account, &borrowed).unwrap();
    assert_eq!(refs.len(), msgs.len());
    for (r, m) in refs.iter().zip(&msgs) {
        assert_eq!(&store.read(r).unwrap(), m);
    }
    store.mark_dead(&refs).unwrap();
    assert!(store.compact(account).unwrap().is_empty(), "nothing live left to remap");

    let bodies: Vec<Vec<u8>> = (0..1000).map(|i| format!("message {i}").into_bytes()).collect();
    let borrowed: Vec<&[u8]> = bodies.iter().map(|b| b.as_slice()).collect();
    let refs = store.append_batch(account, &borrowed).unwrap();
    assert_eq!(refs.len(), 1000);
    for (r, b) in refs.iter().zip(&bodies) {
        assert_eq!(&store.read(r).unwrap(), b);
    }

    // A third or more dead triggers a rewrite; the rest survive it, at a
    // reference [`PackStore::read`] can still open, holding exactly what was
    // written.
    let (dead, live) = refs.split_at(400);
    let live_bodies = &bodies[400..];
    store.mark_dead(dead).unwrap();
    let remap: std::collections::HashMap<PackRef, PackRef> =
        store.compact(account).unwrap().into_iter().collect();
    for (old, body) in live.iter().zip(live_bodies) {
        let r = remap.get(old).unwrap_or(old);
        assert_eq!(&store.read(r).unwrap(), body);
    }
    for old in dead {
        assert!(!remap.contains_key(old), "a dead message must not be remapped as though live");
    }

    store.mark_dead(live).unwrap();
    let _ = store.compact(account);

    eprintln!("--- pack store suite passed ---");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{AeadCipher, NullCipher, SecretKey};

    fn store(dir: &Path, encrypted: bool) -> FilePackStore {
        let cipher: Arc<dyn Cipher> = if encrypted {
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([4u8; 32])))
        } else {
            Arc::new(NullCipher)
        };
        FilePackStore::open(dir, cipher).unwrap()
    }

    #[test]
    fn round_trips_a_handful_of_messages() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let msgs: Vec<&[u8]> = vec![b"From: a\r\n\r\nhello", b"From: b\r\n\r\nworld", b""];
        let refs = s.append_batch("acc-1", &msgs).unwrap();
        assert_eq!(refs.len(), 3);
        for (r, msg) in refs.iter().zip(&msgs) {
            assert_eq!(s.read(r).unwrap(), *msg);
        }
    }

    #[test]
    fn a_batch_of_a_thousand_round_trips_and_shares_one_pack() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let bodies: Vec<Vec<u8>> =
            (0..1000).map(|i| format!("message number {i}").into_bytes()).collect();
        let borrowed: Vec<&[u8]> = bodies.iter().map(|b| b.as_slice()).collect();

        let refs = s.append_batch("acc-big", &borrowed).unwrap();
        assert_eq!(refs.len(), 1000);
        assert!(refs.iter().all(|r| r.pack == refs[0].pack), "one small batch fits one pack");
        for (r, body) in refs.iter().zip(&bodies) {
            assert_eq!(&s.read(r).unwrap(), body);
        }
    }

    #[test]
    fn tampering_with_a_sealed_frame_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs = s.append_batch("acc-1", &[b"untouched".as_slice()]).unwrap();
        let r = &refs[0];

        let path = s.account_dir(&r.account).join(format!("{}.pack", r.pack));
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();

        assert_eq!(s.read(r).unwrap_err().code(), "decrypt_failed");
    }

    #[test]
    fn a_frame_swapped_with_another_in_the_same_pack_is_detected() {
        // The associated data binds a frame to its own offset, not merely to
        // the pack, so physically swapping two same-sized frames -- the one
        // move that does not corrupt the framing -- must still fail to open
        // at either position. Mirrors
        // `blobstore::tests::chunks_cannot_be_swapped_between_blobs`.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs = s.append_batch("acc-1", &[b"AAAAA".as_slice(), b"BBBBB".as_slice()]).unwrap();
        assert_eq!(refs[0].len, refs[1].len, "equal-length plaintexts seal to equal lengths");

        let path = s.account_dir("acc-1").join(format!("{}.pack", refs[0].pack));
        let mut bytes = std::fs::read(&path).unwrap();
        let (a, b) = (refs[0].offset as usize, refs[1].offset as usize);
        let len = refs[0].len as usize;
        let (frame_a, frame_b) = (bytes[a..a + len].to_vec(), bytes[b..b + len].to_vec());
        bytes[a..a + len].copy_from_slice(&frame_b);
        bytes[b..b + len].copy_from_slice(&frame_a);
        std::fs::write(&path, bytes).unwrap();

        assert_eq!(s.read(&refs[0]).unwrap_err().code(), "decrypt_failed");
        assert_eq!(s.read(&refs[1]).unwrap_err().code(), "decrypt_failed");
    }

    #[test]
    fn compaction_keeps_live_messages_and_remaps_their_refs() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let msgs: Vec<&[u8]> = (0..9).map(|_| b"x".as_slice()).collect();
        let refs = s.append_batch("acc-1", &msgs).unwrap();

        // A third or more dead: mark four of nine (>= 1/3) so compaction
        // actually runs.
        let dead: Vec<PackRef> = refs[..4].to_vec();
        let live: Vec<PackRef> = refs[4..].to_vec();
        s.mark_dead(&dead).unwrap();

        let remap = s.compact("acc-1").unwrap();
        assert_eq!(remap.len(), live.len(), "only the live messages should move");

        let remap: std::collections::HashMap<PackRef, PackRef> = remap.into_iter().collect();
        for old in &live {
            let new = remap.get(old).expect("every live ref should be remapped");
            assert_ne!(new.pack, old.pack, "compaction must write a fresh pack");
            assert_eq!(s.read(new).unwrap(), b"x", "the message itself must survive the move");
        }

        // The old pack is gone; reading the stale ref must fail cleanly
        // rather than silently returning something from whatever replaced
        // it on disk.
        assert!(s.read(&dead[0]).is_err());
    }

    #[test]
    fn a_pack_under_the_dead_threshold_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs: Vec<PackRef> =
            s.append_batch("acc-1", &(0..9).map(|_| b"x".as_slice()).collect::<Vec<_>>()).unwrap();

        // Two of nine is under a third: nothing should move.
        s.mark_dead(&refs[..2]).unwrap();
        let remap = s.compact("acc-1").unwrap();
        assert!(remap.is_empty());
        for r in &refs[2..] {
            assert_eq!(s.read(r).unwrap(), b"x");
        }
    }

    #[test]
    fn reopening_after_a_crash_mid_record_recovers_every_complete_message() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs = s.append_batch("acc-1", &[b"first message".as_slice()]).unwrap();

        // Simulate a crash partway through writing a second frame: the
        // length prefix landed, but the sealed bytes after it did not.
        let path = s.account_dir("acc-1").join(format!("{}.pack", refs[0].pack));
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(&999u32.to_le_bytes());
        bytes.extend_from_slice(b"only some of the next fra");
        std::fs::write(&path, &bytes).unwrap();

        // "Reopening" -- a fresh store over the same directory, as a
        // restarted process would construct.
        let reopened = store(dir.path(), true);
        assert_eq!(
            reopened.read(&refs[0]).unwrap(),
            b"first message",
            "a message written and flushed before the crash must survive it"
        );

        // Appending after the crash must not leave the torn tail sitting in
        // the middle of the file: the next batch has to land where the last
        // complete frame ended, not after the garbage.
        let more = reopened.append_batch("acc-1", &[b"after the crash".as_slice()]).unwrap();
        assert_eq!(reopened.read(&more[0]).unwrap(), b"after the crash");
        assert_eq!(reopened.read(&refs[0]).unwrap(), b"first message", "still there afterwards");

        let on_disk = std::fs::read(&path).unwrap();
        assert!(
            !on_disk.windows(25).any(|w| w == b"only some of the next fra"),
            "the torn tail must be truncated away, not merely skipped over"
        );
    }

    #[test]
    fn packs_rotate_once_they_reach_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let big = vec![0u8; (PACK_ROTATE_BYTES as usize) + 1];

        let first = s.append_batch("acc-1", &[big.as_slice()]).unwrap();
        let second = s.append_batch("acc-1", &[b"tiny".as_slice()]).unwrap();
        assert_ne!(first[0].pack, second[0].pack, "a full pack must not take more writes");
        assert_eq!(s.read(&second[0]).unwrap(), b"tiny");
    }

    #[test]
    fn an_empty_batch_touches_no_storage() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        assert!(s.append_batch("acc-1", &[]).unwrap().is_empty());
        assert!(!s.account_dir("acc-1").exists());
    }

    #[test]
    fn a_pack_written_under_one_key_does_not_open_under_another() {
        let dir = tempfile::tempdir().unwrap();
        let refs = store(dir.path(), true).append_batch("acc-1", &[b"private".as_slice()]).unwrap();

        let other = FilePackStore::open(
            dir.path(),
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([9u8; 32]))),
        )
        .unwrap();
        assert_eq!(other.read(&refs[0]).unwrap_err().code(), "decrypt_failed");
    }

    #[test]
    fn passes_the_shared_conformance_suite() {
        let dir = tempfile::tempdir().unwrap();
        run_pack_store_suite(&store(dir.path(), true), "acc-conformance");
    }
}

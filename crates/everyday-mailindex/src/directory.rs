//! A tantivy [`Directory`] whose files are sealed with the same
//! XChaCha20-Poly1305 envelope as everything else in the vault.
//!
//! # Why not [`MmapDirectory`](tantivy::directory::MmapDirectory)
//!
//! tantivy's own default directory maps a segment file's bytes straight
//! into the process's address space and hands readers slices of that
//! mapping directly: the contract a reader relies on is that what is on
//! disk *is* what a query is allowed to see. That contract is exactly what
//! a vault cannot offer — every byte this crate writes has to be
//! ciphertext at rest, and there is no page-fault hook to decrypt a slice
//! on the way out of a memory-mapped read. Wrapping `MmapDirectory` was
//! never on the table: either the seal would have to be removed before the
//! bytes reached disk, defeating the point, or every consumer of a mapped
//! slice — deep inside tantivy's own segment readers — would need to know
//! about decryption, which the `Directory` trait gives no way to arrange.
//!
//! So this directory does the opposite of what a memory-mapped one does:
//! rather than lazily faulting in whichever pages a query happens to touch,
//! [`SealedDirectory::get_file_handle`] decrypts a *whole* segment file the
//! first time anything asks to read it, and [`cache::DecryptedCache`] is
//! what stops that from happening again for the rest of the session. This
//! is a real trade, not a free lunch:
//!
//! - **Where it loses to mmap.** A query that only ever touches a slice of
//!   a large segment still pays to decrypt the whole thing on first touch,
//!   where a mapped file would only fault in the pages actually read.
//!   Memory is a cache this crate manages explicitly, with a byte cap the
//!   constructor is given, rather than pages the OS reclaims under memory
//!   pressure on its own initiative; and two open `SealedDirectory`s over
//!   the same on-disk files (unusual, but not impossible — a second process
//!   inspecting the same vault read-only) each keep their own decrypted
//!   copy, where mmap's page cache is shared automatically by the OS.
//! - **Where it does not.** Segment files are immutable once tantivy
//!   finishes writing one — the whole index is a write-once, append-more
//!   structure at the segment level — so "decrypt once per session" is not
//!   an approximation of mmap's steady state, it *is* the steady state:
//!   after the first read of a given segment, this directory is exactly as
//!   fast as a mapped one, because there is no decryption left to do. The
//!   risk table in `docs/plans/mail.md` names this trade explicitly and
//!   asks for it to be measured against the search budget; see
//!   `tests/benchmark.rs`.
//!
//! # What is sealed, and how
//!
//! - **Segment files** — everything tantivy opens with
//!   [`Directory::open_write`] — are immutable once written, so they are
//!   sealed whole, once, when tantivy calls
//!   [`TerminatingWrite::terminate`]. There is no incremental seal-as-you-go
//!   here: the plaintext is buffered in memory as it streams in (segments
//!   are bounded by the writer's heap budget, so this is not unbounded) and
//!   sealed in one call once the last byte has arrived, which is both
//!   simpler and cheaper than re-sealing a growing prefix on every flush.
//! - **`meta.json` and `.managed.json`** go through
//!   [`Directory::atomic_write`], tantivy's own "replace this small file
//!   without a reader ever observing a half-written one" API. This
//!   directory answers it with exactly the sequence
//!   [`everyday_core::fsutil::write_atomic`] already gives every other
//!   small control file in the vault: seal, write to a temp file, `fsync`
//!   it, rename over the target, `fsync` the directory. Reusing that
//!   helper rather than re-deriving the same five steps is deliberate —
//!   see that function's docs for why each step is there.
//! - **Lockfiles** (`.tantivy-writer.lock`, `.tantivy-meta.lock`) are
//!   written in the clear. They hold no content — their entire purpose is
//!   to exist or not — so sealing them would spend an AEAD tag protecting
//!   zero bits of secret, and `Directory::acquire_lock`'s default
//!   implementation relies on `open_write` failing with
//!   `FileAlreadyExists` for exclusivity, which a plain `create_new` open
//!   gives for free.
//!
//! Every seal binds the file's own relative path as associated data (see
//! [`file_aad`]), the same "name it belongs to" binding
//! [`everyday_core::packstore`] uses for pack frames: a segment file moved
//! or renamed onto another one's path — the one filesystem-level attack a
//! detached AEAD tag alone does not stop — fails to open rather than
//! decrypting as though nothing had happened.
//!
//! # `watch`
//!
//! tantivy uses `watch` for exactly one thing: `ReloadPolicy::OnCommit`
//! reload, which fires whenever `meta.json` changes. This directory does
//! not poll for that the way [`MmapDirectory`](tantivy::directory::MmapDirectory)
//! does on platforms without inotify — it already sees every write to
//! `meta.json` pass through its own [`Directory::atomic_write`], so it
//! calls the registered callbacks directly from there, synchronously and
//! for free. See [`RamDirectory`](tantivy::directory::RamDirectory) for
//! prior art doing the same thing for the same reason.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use everyday_core::crypto::Cipher;
use everyday_core::error::{Error, Result as CoreResult};
use everyday_core::fsutil;
use tantivy::directory::error::{DeleteError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, Directory, FileHandle, OwnedBytes, TerminatingWrite, WatchCallback,
    WatchCallbackList, WatchHandle, WritePtr,
};

use crate::cache::DecryptedCache;

/// The name tantivy writes its commit manifest under. Not exported by
/// tantivy itself (`crate::core::META_FILEPATH` is private to that crate),
/// so it is repeated here — safe to, since it is part of tantivy's on-disk
/// format, not an implementation detail that could change under a pinned
/// exact version.
const META_FILE: &str = "meta.json";

/// A directory whose every file is sealed. See the module docs.
pub struct SealedDirectory {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    cipher: Arc<dyn Cipher>,
    cache: Mutex<DecryptedCache>,
    watch_router: WatchCallbackList,
}

impl Clone for SealedDirectory {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl fmt::Debug for SealedDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedDirectory").field("root", &self.inner.root).finish()
    }
}

impl SealedDirectory {
    /// Open (creating if necessary) a sealed directory rooted at `root`,
    /// keeping up to `cache_bytes` of decrypted segment content in memory
    /// at once. See [`cache::DecryptedCache`] for the eviction policy.
    pub fn open(
        root: impl Into<PathBuf>,
        cipher: Arc<dyn Cipher>,
        cache_bytes: usize,
    ) -> CoreResult<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                cipher,
                cache: Mutex::new(DecryptedCache::new(cache_bytes)),
                watch_router: WatchCallbackList::default(),
            }),
        })
    }

    fn full_path(&self, path: &Path) -> PathBuf {
        self.inner.root.join(path)
    }
}

fn is_lock_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "lock")
}

/// Associated data binding a sealed file to the relative path tantivy knows
/// it by. See the module docs' "what is sealed, and how".
fn file_aad(path: &Path) -> Vec<u8> {
    format!("everyday.mailindex.v1:{}", path.to_string_lossy()).into_bytes()
}

fn decrypt_failed_err(path: &Path) -> OpenReadError {
    OpenReadError::wrap_io_error(
        io::Error::other("decryption failed: the data is corrupt or the wrong key was used"),
        path.to_path_buf(),
    )
}

/// A writer for a lockfile: no sealing, `sync_data` on close. See the
/// module docs on why lockfiles stay plain.
struct PlainWriter(File);

impl Write for PlainWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminatingWrite for PlainWriter {
    fn terminate_ref(&mut self, _: AntiCallToken) -> io::Result<()> {
        self.0.flush()?;
        self.0.sync_data()
    }
}

/// A writer for a segment file: buffers plaintext in memory, seals and
/// writes it whole when tantivy is done with it. See the module docs'
/// "what is sealed, and how" for why sealing happens once, at the end,
/// rather than incrementally.
struct SealedSegmentWriter {
    directory: SealedDirectory,
    rel_path: PathBuf,
    full_path: PathBuf,
    buffer: Vec<u8>,
}

impl Write for SealedSegmentWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        // Nothing to durably commit yet -- sealing happens once, whole, in
        // `terminate_ref`. tantivy is documented not to rely on `Drop` for
        // this, and never calls `flush` expecting durability on its own.
        Ok(())
    }
}

impl TerminatingWrite for SealedSegmentWriter {
    fn terminate_ref(&mut self, _: AntiCallToken) -> io::Result<()> {
        let sealed = self
            .directory
            .inner
            .cipher
            .seal(&file_aad(&self.rel_path), &self.buffer)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut file = File::create(&self.full_path)?;
        file.write_all(&sealed)?;
        file.sync_all()?;
        Ok(())
    }
}

impl Directory for SealedDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        let full = self.full_path(path);

        if is_lock_file(path) {
            let bytes = std::fs::read(&full).map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    OpenReadError::FileDoesNotExist(path.to_path_buf())
                } else {
                    OpenReadError::wrap_io_error(e, path.to_path_buf())
                }
            })?;
            return Ok(Arc::new(OwnedBytes::new(bytes)));
        }

        if let Some(cached) = {
            let mut cache = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.get(path)
        } {
            return Ok(Arc::new(OwnedBytes::new(cached)));
        }

        let sealed = std::fs::read(&full).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                OpenReadError::FileDoesNotExist(path.to_path_buf())
            } else {
                OpenReadError::wrap_io_error(e, path.to_path_buf())
            }
        })?;
        let plain = self
            .inner
            .cipher
            .open(&file_aad(path), &sealed)
            .map_err(|_| decrypt_failed_err(path))?;
        let plain: Arc<[u8]> = Arc::from(plain);
        {
            let mut cache = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.insert(path.to_path_buf(), Arc::clone(&plain));
        }
        Ok(Arc::new(OwnedBytes::new(plain)))
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        let full = self.full_path(path);
        std::fs::remove_file(&full).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                DeleteError::FileDoesNotExist(path.to_path_buf())
            } else {
                DeleteError::IoError { io_error: Arc::new(e), filepath: path.to_path_buf() }
            }
        })?;
        let mut cache = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.remove(path);
        Ok(())
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.full_path(path)
            .try_exists()
            .map_err(|e| OpenReadError::wrap_io_error(e, path.to_path_buf()))
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let full = self.full_path(path);
        if full.exists() {
            return Err(OpenWriteError::FileAlreadyExists(path.to_path_buf()));
        }

        if is_lock_file(path) {
            let file =
                OpenOptions::new().write(true).create_new(true).open(&full).map_err(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        OpenWriteError::FileAlreadyExists(path.to_path_buf())
                    } else {
                        OpenWriteError::wrap_io_error(e, path.to_path_buf())
                    }
                })?;
            return Ok(BufWriter::new(Box::new(PlainWriter(file))));
        }

        Ok(BufWriter::new(Box::new(SealedSegmentWriter {
            directory: self.clone(),
            rel_path: path.to_path_buf(),
            full_path: full,
            buffer: Vec::new(),
        })))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        let full = self.full_path(path);
        let sealed = std::fs::read(&full).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                OpenReadError::FileDoesNotExist(path.to_path_buf())
            } else {
                OpenReadError::wrap_io_error(e, path.to_path_buf())
            }
        })?;
        self.inner.cipher.open(&file_aad(path), &sealed).map_err(|_| decrypt_failed_err(path))
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let full = self.full_path(path);
        let sealed = self
            .inner
            .cipher
            .seal(&file_aad(path), data)
            .map_err(|e| io::Error::other(e.to_string()))?;
        fsutil::write_atomic(&full, &sealed, &fsutil::unique_tag())
            .map_err(|e| io::Error::other(e.to_string()))?;
        if path == Path::new(META_FILE) {
            // See the module docs' `watch` section: this is the one and
            // only trigger tantivy needs, and we already see every write.
            drop(self.inner.watch_router.broadcast());
        }
        Ok(())
    }

    // `acquire_lock` is not overridden: the trait's default implementation
    // -- retrying `open_write` on the lock's path and relying on
    // `FileAlreadyExists` for exclusivity -- is exactly right for our plain
    // lockfiles, and `open_write` above already gives it that.

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        Ok(self.inner.watch_router.subscribe(watch_callback))
    }

    fn sync_directory(&self) -> io::Result<()> {
        fsutil::sync_dir(&self.inner.root);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::crypto::{AeadCipher, NullCipher, SecretKey};
    use tantivy::directory::Lock;
    use tantivy::directory::error::LockError;

    fn cipher() -> Arc<dyn Cipher> {
        Arc::new(AeadCipher::new(&SecretKey::from_bytes([3u8; 32])))
    }

    fn dir() -> (tempfile::TempDir, SealedDirectory) {
        let tmp = tempfile::tempdir().unwrap();
        let d = SealedDirectory::open(tmp.path(), cipher(), 1024 * 1024).unwrap();
        (tmp, d)
    }

    fn write_all(d: &SealedDirectory, path: &Path, content: &[u8]) {
        let mut w = d.open_write(path).unwrap();
        w.write_all(content).unwrap();
        // `flush` alone does not seal and persist the file -- see
        // `SealedSegmentWriter::flush`'s docs. `terminate` is what tantivy
        // itself always calls when it is done writing a file.
        w.terminate().unwrap();
        d.sync_directory().unwrap();
    }

    #[test]
    fn writes_and_reads_a_segment_file_back() {
        let (_tmp, d) = dir();
        let path = Path::new("0.term");
        write_all(&d, path, b"hello segment");
        let handle = d.get_file_handle(path).unwrap();
        let bytes = handle.read_bytes(0..handle.len()).unwrap();
        assert_eq!(bytes.as_slice(), b"hello segment");
    }

    #[test]
    fn the_file_on_disk_is_not_the_plaintext() {
        let (tmp, d) = dir();
        let path = Path::new("0.term");
        write_all(&d, path, b"a secret nobody should read off disk");
        let on_disk = std::fs::read(tmp.path().join(path)).unwrap();
        assert!(!on_disk.windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn a_second_write_to_the_same_path_fails_worm() {
        let (_tmp, d) = dir();
        let path = Path::new("0.term");
        write_all(&d, path, b"first");
        let err = match d.open_write(path) {
            Err(e) => e,
            Ok(_) => panic!("expected FileAlreadyExists"),
        };
        assert!(matches!(err, OpenWriteError::FileAlreadyExists(_)));
    }

    #[test]
    fn atomic_write_then_atomic_read_round_trips() {
        let (_tmp, d) = dir();
        let path = Path::new(META_FILE);
        d.atomic_write(path, b"{\"generation\":1}").unwrap();
        assert_eq!(d.atomic_read(path).unwrap(), b"{\"generation\":1}");
    }

    #[test]
    fn atomic_write_replaces_the_previous_content() {
        let (_tmp, d) = dir();
        let path = Path::new(".managed.json");
        d.atomic_write(path, b"[]").unwrap();
        d.atomic_write(path, b"[\"0.term\"]").unwrap();
        assert_eq!(d.atomic_read(path).unwrap(), b"[\"0.term\"]");
    }

    #[test]
    fn atomic_write_to_meta_json_wakes_a_watcher() {
        let (_tmp, d) = dir();
        let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen_clone = Arc::clone(&seen);
        let _handle = d
            .watch(WatchCallback::new(move || {
                seen_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            }))
            .unwrap();
        d.atomic_write(Path::new(META_FILE), b"{}").unwrap();
        // The broadcast runs on its own thread; give it a moment.
        for _ in 0..200 {
            if seen.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(seen.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn a_missing_file_is_reported_clearly() {
        let (_tmp, d) = dir();
        let err = d.get_file_handle(Path::new("nope.term")).unwrap_err();
        assert!(matches!(err, OpenReadError::FileDoesNotExist(_)));
        let err = d.atomic_read(Path::new("nope.json")).unwrap_err();
        assert!(matches!(err, OpenReadError::FileDoesNotExist(_)));
        let err = d.delete(Path::new("nope.term")).unwrap_err();
        assert!(matches!(err, DeleteError::FileDoesNotExist(_)));
    }

    #[test]
    fn delete_removes_a_file_and_its_cache_entry() {
        let (_tmp, d) = dir();
        let path = Path::new("0.term");
        write_all(&d, path, b"gone soon");
        assert!(d.get_file_handle(path).is_ok(), "primes the cache");
        d.delete(path).unwrap();
        assert!(matches!(d.exists(path), Ok(false)));
        assert!(matches!(d.get_file_handle(path).unwrap_err(), OpenReadError::FileDoesNotExist(_)));
    }

    #[test]
    fn tampering_with_a_sealed_segment_is_detected() {
        let (tmp, d) = dir();
        let path = Path::new("0.term");
        write_all(&d, path, b"untouched content");
        let full = tmp.path().join(path);
        let mut bytes = std::fs::read(&full).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&full, bytes).unwrap();
        let err = d.get_file_handle(path).unwrap_err();
        assert!(matches!(err, OpenReadError::IoError { .. }));
    }

    #[test]
    fn a_segment_renamed_onto_another_paths_name_does_not_open() {
        // The associated data binds a file to its own relative path, so
        // copying valid ciphertext to a different filename must not decrypt
        // there -- the one thing a detached tag alone would not catch.
        let (tmp, d) = dir();
        write_all(&d, Path::new("a.term"), b"segment a");
        write_all(&d, Path::new("b.term"), b"segment b");
        let a_bytes = std::fs::read(tmp.path().join("a.term")).unwrap();
        std::fs::write(tmp.path().join("b.term"), a_bytes).unwrap();
        // Force a fresh read rather than serving the cached plaintext for
        // "b.term" from before we clobbered the file on disk.
        d.delete(Path::new("does-not-exist")).ok();
        let d2 = SealedDirectory::open(tmp.path(), cipher(), 1024 * 1024).unwrap();
        let err = d2.get_file_handle(Path::new("b.term")).unwrap_err();
        assert!(matches!(err, OpenReadError::IoError { .. }));
    }

    #[test]
    fn opening_with_the_wrong_key_fails_clearly() {
        let (tmp, d) = dir();
        write_all(&d, Path::new("0.term"), b"only readable with the right key");
        let other = SealedDirectory::open(
            tmp.path(),
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([9u8; 32]))),
            1024 * 1024,
        )
        .unwrap();
        let err = other.get_file_handle(Path::new("0.term")).unwrap_err();
        assert!(matches!(err, OpenReadError::IoError { .. }));
    }

    #[test]
    fn a_null_cipher_directory_still_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let d = SealedDirectory::open(tmp.path(), Arc::new(NullCipher), 1024).unwrap();
        write_all(&d, Path::new("0.term"), b"plain");
        let handle = d.get_file_handle(Path::new("0.term")).unwrap();
        assert_eq!(handle.read_bytes(0..handle.len()).unwrap().as_slice(), b"plain");
    }

    #[test]
    fn lockfiles_are_plain_on_disk_and_enforce_exclusivity() {
        let (tmp, d) = dir();
        let lock = Lock { filepath: PathBuf::from(".tantivy-writer.lock"), is_blocking: false };
        let held = d.acquire_lock(&lock).unwrap();
        assert_eq!(std::fs::read(tmp.path().join(&lock.filepath)).unwrap(), b"");
        assert!(matches!(d.acquire_lock(&lock), Err(LockError::LockBusy)));
        drop(held);
        assert!(d.acquire_lock(&lock).is_ok());
    }

    #[test]
    fn exists_reports_created_and_missing_files_correctly() {
        let (_tmp, d) = dir();
        assert!(!d.exists(Path::new("0.term")).unwrap());
        write_all(&d, Path::new("0.term"), b"here now");
        assert!(d.exists(Path::new("0.term")).unwrap());
    }

    #[test]
    fn reads_a_slice_of_a_cached_segment() {
        let (_tmp, d) = dir();
        let path = Path::new("0.term");
        write_all(&d, path, b"0123456789");
        // First read primes the cache; second exercises the cached path.
        let _ = d.get_file_handle(path).unwrap();
        let handle = d.get_file_handle(path).unwrap();
        assert_eq!(handle.read_bytes(2..5).unwrap().as_slice(), b"234");
    }
}

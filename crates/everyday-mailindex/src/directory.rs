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
//!   here: the plaintext is buffered in memory as it streams in and sealed
//!   in one call once the last byte has arrived, which is both simpler and
//!   cheaper than re-sealing a growing prefix on every flush. `seal` itself
//!   then allocates a second, full-size copy for the sealed bytes, so one
//!   segment briefly costs twice its own size in memory — [`SealedSegmentWriter::terminate_ref`]
//!   frees the plaintext half as soon as the sealed copy exists, rather
//!   than holding both for the length of the write that follows, but the
//!   peak during `seal` itself is still two full copies at once. This is
//!   bounded by [`crate::index::WRITER_HEAP_BYTES`] for an *ordinary*
//!   segment tantivy's own indexing thread produces, but **not** for a
//!   segment tantivy merges from several existing ones: a merge writes
//!   through this same `open_write`, and a merged segment over a
//!   hundred-thousand-message mailbox can be far larger than any one
//!   batch's own writer-heap budget. Streaming the seal in fixed-size
//!   chunks instead would remove the doubling entirely, at the cost of a
//!   framed, chunked file format this crate does not have today — left as
//!   a known, documented limitation rather than built speculatively ahead
//!   of a mailbox actually large enough for it to matter in practice.
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
//!   zero bits of secret. What decides exclusivity is not their content, or
//!   even their existence: [`SealedDirectory::acquire_lock`] overrides the
//!   trait's own default (below) to take a real OS advisory lock on the
//!   open file, the same primitive [`MmapDirectory`](tantivy::directory::MmapDirectory)
//!   itself reaches for and for the same reason — a lock that is *merely* a
//!   file existing, released only by a caller's own `Drop` running, does
//!   not survive a `SIGKILL` or a power loss: the file is still there on
//!   the next open, and every open after that, forever. An OS-held lock is
//!   released by the kernel the moment the holding process's file
//!   descriptors go away, no destructor required, which is what makes a
//!   lock left behind by a crash recoverable without anyone noticing and
//!   deleting a file by hand. See [`os_lock`]'s own docs for the platform
//!   split — Unix gets the real fix, non-Unix keeps the trait's original
//!   behaviour rather than an untested lock primitive.
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
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, Directory, DirectoryLock, FileHandle, Lock, OwnedBytes, TerminatingWrite,
    WatchCallback, WatchCallbackList, WatchHandle, WritePtr,
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

/// An OS-kernel advisory lock on an open file, the same primitive
/// [`MmapDirectory`](tantivy::directory::MmapDirectory) reaches for through
/// the `fs4` crate -- reimplemented here as a direct `flock(2)` binding,
/// rather than depending on that crate from this one, since `fs4` is
/// already linked into this binary anyway (`everyday-core` depends on it
/// directly, for the vault's own [`everyday_core::lockfile`]) and a second
/// copy buys nothing a few lines of `extern "C"` do not already give for
/// free. The one property this exists for: the kernel drops the lock the
/// moment the holding process's file descriptor table goes away, crash or
/// clean exit alike, which is what makes a lock left by a killed process
/// recoverable without anyone having to notice and delete a file by hand.
///
/// Windows is not covered -- `SealedDirectory::acquire_lock` falls back to
/// tantivy's own default (create-the-file, delete-on-drop) there, exactly
/// the behaviour this module exists to move away from on the platforms
/// this vault is actually tested on. A `LockFileEx`/`UnlockFileEx` binding
/// would close that gap the same way, if this ever needs to run on Windows
/// without Tauri's own desktop shell mediating file access some other way.
#[cfg(unix)]
mod os_lock {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;

    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    pub enum AcquireError {
        /// Already held by someone else -- `EWOULDBLOCK`, from `LOCK_NB`.
        WouldBlock,
        Io(io::Error),
    }

    /// Open `path` (creating it if it does not exist -- including
    /// reopening a stale one a killed process left behind) and take an
    /// exclusive, non-blocking advisory lock on it.
    ///
    /// The returned `File` *is* the lock: held for as long as the caller
    /// keeps it, released by the kernel the instant every descriptor on
    /// this same open file description closes -- a clean drop, an
    /// unhandled panic unwinding past it, or a `SIGKILL` that runs no
    /// destructor at all. Nothing here ever deletes `path`: the next
    /// `acquire` reopens the very same file and lets the kernel decide
    /// whether it is actually free, never a directory listing that a
    /// crash could leave stale.
    pub fn acquire(path: &Path) -> Result<File, AcquireError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(AcquireError::Io)?;
        // Safety: `file.as_raw_fd()` is a valid, open file descriptor for
        // as long as `file` is alive, which outlives this call; `flock`
        // takes no pointers and cannot violate memory safety regardless of
        // what it returns.
        let ret = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if ret == 0 {
            return Ok(file);
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            Err(AcquireError::WouldBlock)
        } else {
            Err(AcquireError::Io(err))
        }
    }
}

/// The non-Unix stand-in: tantivy's own default `acquire_lock` strategy
/// (exclusive by atomic creation, cleaned up by deleting the file on drop),
/// inlined here so [`SealedDirectory::acquire_lock`] needs only one call
/// regardless of platform -- see [`os_lock`]'s own docs for why only Unix
/// gets the actual fix in this module. Not crash-safe: a lock left behind
/// by a killed process on this platform still has to be removed by hand,
/// exactly as it did before this fix.
#[cfg(not(unix))]
mod os_lock {
    use std::fs::OpenOptions;
    use std::io;
    use std::path::{Path, PathBuf};

    pub enum AcquireError {
        WouldBlock,
        Io(io::Error),
    }

    pub struct DeleteOnDrop(PathBuf);

    impl Drop for DeleteOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    pub fn acquire(path: &Path) -> Result<DeleteOnDrop, AcquireError> {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(_file) => Ok(DeleteOnDrop(path.to_path_buf())),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(AcquireError::WouldBlock),
            Err(e) => Err(AcquireError::Io(e)),
        }
    }
}

/// How many more times a blocking lock is tried after the first refusal,
/// and how long apart. Tantivy's own default for a blocking lock, copied
/// rather than reinvented: ten seconds is far longer than any reader reload
/// or garbage collection holds `META_LOCK`, and short enough that a genuinely
/// stuck holder surfaces as an error rather than a hang.
const LOCK_RETRIES: usize = 100;
const LOCK_RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

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
        // `seal` above already paid for a second, full-size copy -- see the
        // module docs on this writer's memory shape. Freeing the plaintext
        // half here, before the write below, is the one mitigation
        // available without a chunked/streaming seal: it does not lower the
        // peak `seal` itself reaches, but it stops that peak from being
        // held for the length of a write syscall too, which is what a slow
        // disk (or a merged segment far larger than one batch's own
        // writer-heap budget) turns from "a spike" into "a spike that
        // lasts".
        self.buffer = Vec::new();
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

    /// Overridden, unlike every other method here that just delegates to a
    /// plain file -- see the module docs' "Why a real OS lock, not a plain
    /// file" for why the trait's own default (create the file exclusively,
    /// delete it on drop) is exactly the mistake
    /// [`MmapDirectory`](tantivy::directory::MmapDirectory) itself does not
    /// make.
    ///
    /// What it keeps from the default is the waiting. A [`Lock`] says
    /// whether its caller expects to wait for it, and tantivy's
    /// `META_LOCK` does: a reader reloading after a commit and the garbage
    /// collection after a background merge both take it, briefly, and
    /// each counts on the other finishing. Refusing at once turned an
    /// ordinary overlap into a `commit` that reported `LockBusy` after its
    /// write had succeeded, and into merged-away segments nobody deleted.
    /// So a blocking lock is retried on the default's own schedule —
    /// [`LOCK_RETRIES`] times, [`LOCK_RETRY_WAIT`] apart — and only a
    /// non-blocking one, the writer's, is refused on the first try.
    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        let full = self.full_path(&lock.filepath);
        let mut retries = if lock.is_blocking { LOCK_RETRIES } else { 0 };
        loop {
            match os_lock::acquire(&full) {
                Ok(guard) => return Ok(DirectoryLock::from(Box::new(guard))),
                Err(os_lock::AcquireError::WouldBlock) if retries > 0 => {
                    retries -= 1;
                    std::thread::sleep(LOCK_RETRY_WAIT);
                }
                Err(os_lock::AcquireError::WouldBlock) => return Err(LockError::LockBusy),
                Err(os_lock::AcquireError::Io(e)) => return Err(LockError::IoError(Arc::new(e))),
            }
        }
    }

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

    /// Regression for "a crash leaves a lock file that blocks every future
    /// open": under the trait's own default `acquire_lock` (create the
    /// file exclusively, delete it on drop), a lock file merely *existing*
    /// -- left behind by a process that was `SIGKILL`ed before its `Drop`
    /// ever ran -- would refuse every future open with `LockBusy` forever,
    /// since nothing left running could ever delete it. The OS advisory
    /// lock this module actually takes does not care whether the file
    /// already exists on disk, only whether the kernel currently
    /// associates a lock with it -- which a file nobody has `flock`ed,
    /// however it got there, never does.
    #[test]
    fn a_stale_lock_file_left_by_a_dead_process_is_not_an_obstacle() {
        let (tmp, d) = dir();
        let lock = Lock { filepath: PathBuf::from(".tantivy-writer.lock"), is_blocking: false };

        // A file on disk with no live kernel lock on it at all -- standing
        // in for exactly what a crashed process leaves behind, since this
        // never calls `acquire_lock` (or `flock`) to create it.
        std::fs::write(tmp.path().join(&lock.filepath), b"leftover from a killed process").unwrap();

        let acquired = d.acquire_lock(&lock);
        assert!(
            acquired.is_ok(),
            "a stale lock file with no live OS lock on it must still be claimable, not \
             permanently refuse every future open"
        );

        // The lock just taken still behaves like every other one: exclusive
        // while held, and releasable.
        assert!(matches!(d.acquire_lock(&lock), Err(LockError::LockBusy)));
        drop(acquired);
        assert!(d.acquire_lock(&lock).is_ok());
    }

    /// Regression for a commit that failed after it had written: tantivy
    /// takes `META_LOCK` both to reload a reader and to collect garbage
    /// after a merge, marks it blocking, and expects the second of two
    /// overlapping takers to wait rather than be refused.
    #[test]
    fn a_blocking_lock_waits_for_its_holder_instead_of_refusing() {
        let (_tmp, d) = dir();
        let meta = Lock { filepath: PathBuf::from(".tantivy-meta.lock"), is_blocking: true };
        let held = d.acquire_lock(&meta).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            drop(held);
        });
        let waited = std::time::Instant::now();
        assert!(d.acquire_lock(&meta).is_ok(), "a blocking lock must wait for its holder");
        assert!(waited.elapsed() >= std::time::Duration::from_millis(200));
        releaser.join().unwrap();

        // The writer's lock is not blocking, and is still refused at once:
        // two writers is a mistake to report, not a queue to join.
        let writer = Lock { filepath: PathBuf::from(".tantivy-writer.lock"), is_blocking: false };
        let _held = d.acquire_lock(&writer).unwrap();
        let tried = std::time::Instant::now();
        assert!(matches!(d.acquire_lock(&writer), Err(LockError::LockBusy)));
        assert!(tried.elapsed() < LOCK_RETRY_WAIT);
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

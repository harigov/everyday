//! The write lock on a vault directory.
//!
//! Two processes writing one vault is not a theoretical problem. The desktop
//! app and `everyday` on the command line are the same core over the same
//! files, and nothing stopped them both being open on the same vault at once
//! -- nor stopped a second copy of the app being launched onto it. SQLite
//! keeps its own file from being corrupted by that, but nothing kept the two
//! from overwriting each other's records: each holds its own in-memory copy
//! of an entry, and the second to save wins, silently.
//!
//! # Why a lock rather than a refusal
//!
//! The lock is *advisory and exclusive*, taken once when a vault is opened
//! and held until it is dropped. Whoever gets it may write. Whoever does not
//! still gets to **open the vault read-only** rather than being turned away:
//! `everyday list` while the app is open is a reasonable thing to want, and
//! it cannot lose anything. Only the writes are refused, with
//! [`Error::VaultInUse`], which names what is holding it.
//!
//! # Why the pid is in the file
//!
//! The lock itself is the kernel's, not the file's contents -- an OS lock is
//! released when the holding process dies, which is what makes this safe
//! across a crash where a bare "lock file exists" check would leave a vault
//! permanently unopenable. The pid and host written inside are only so the
//! error message can say *who*, and are never trusted for the decision.

use crate::error::{Error, Result};
use fs4::{FileExt, TryLockError};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const LOCK_FILENAME: &str = "vault.lock";

/// An exclusive claim on a vault directory, released when dropped.
#[derive(Debug)]
pub struct VaultLock {
    /// Holding the handle is what holds the lock; the OS releases it when
    /// this closes, including if the process dies without unwinding.
    _file: File,
}

/// Who is holding the lock, for the error message.
#[derive(Debug, Clone)]
pub struct LockHolder {
    pub pid: u32,
    pub host: String,
}

impl std::fmt::Display for LockHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "process {} on {}", self.pid, self.host)
    }
}

/// Try to claim `dir` for writing.
///
/// `Ok(Some(lock))` means it is ours. `Ok(None)` means someone else has it
/// and this process should open read-only. An `Err` is a real filesystem
/// problem -- a read-only medium, a directory that does not exist -- and is
/// worth reporting rather than silently degrading, because it means the
/// vault could not have been written to anyway.
pub fn acquire(dir: &Path) -> Result<Option<VaultLock>> {
    let path = dir.join(LOCK_FILENAME);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| Error::io(&path, e))?;

    match FileExt::try_lock(&file) {
        Ok(()) => {}
        // Held by someone else. Not an error here: the caller decides
        // whether read-only is good enough for what it wants to do.
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => return Err(Error::io(&path, e)),
    }

    // Ours. Record who we are, for the next process's error message. Failing
    // to write this must not fail the open -- we hold the lock either way,
    // and a vault on a medium that will not take this one line is a problem
    // that will announce itself soon enough.
    let _ = (|| -> std::io::Result<()> {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        write!(file, "{} {}", std::process::id(), hostname())?;
        file.flush()
    })();

    Ok(Some(VaultLock { _file: file }))
}

/// Read who claims to hold the lock on `dir`, for a message.
///
/// Best-effort and never trusted: the contents are a courtesy, the lock is
/// the kernel's. A missing or unparseable file yields `None` and the caller
/// says "another process" instead of naming one.
pub fn holder(dir: &Path) -> Option<LockHolder> {
    let mut buf = String::new();
    File::open(dir.join(LOCK_FILENAME)).ok()?.read_to_string(&mut buf).ok()?;
    let (pid, host) = buf.trim().split_once(' ')?;
    Some(LockHolder { pid: pid.parse().ok()?, host: host.to_string() })
}

/// Best-effort machine name, so a vault in a synced folder can say which
/// laptop left it open. Never fails: a vault must open on a machine whose
/// hostname we cannot read.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_caller_gets_the_lock_and_the_second_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire(dir.path()).unwrap();
        assert!(first.is_some(), "an unlocked vault must be claimable");

        let second = acquire(dir.path()).unwrap();
        assert!(second.is_none(), "a second claim on a held vault must not succeed");
    }

    #[test]
    fn dropping_the_lock_releases_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire(dir.path()).unwrap();
        assert!(first.is_some());
        drop(first);

        // This is what makes a crash survivable: the claim is the file
        // handle, so it goes away with the process that held it.
        assert!(acquire(dir.path()).unwrap().is_some(), "the lock must be reclaimable");
    }

    #[test]
    fn the_holder_is_recorded_for_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = acquire(dir.path()).unwrap().unwrap();
        let holder = holder(dir.path()).expect("the holder should be readable");
        assert_eq!(holder.pid, std::process::id());
        assert!(!holder.host.is_empty());
    }

    #[test]
    fn a_stale_lock_file_from_a_dead_process_is_not_an_obstacle() {
        // The failure mode this design exists to avoid: a lock that is a mere
        // file, left behind by a crash, that nobody can clear.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOCK_FILENAME), "999999 some-dead-host").unwrap();
        assert!(acquire(dir.path()).unwrap().is_some(), "a leftover file must not block a vault");
    }
}

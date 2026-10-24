//! Durable file writes.
//!
//! Everything in a vault that is not inside SQLite is written through here,
//! because "temp file plus rename" on its own is only half of the pattern.
//! A rename is atomic with respect to *readers* -- nobody ever sees a
//! half-written file -- but it says nothing about what survives a power cut.
//! Without the fsyncs below, a crash can leave the rename durable and the
//! bytes it points at not, which is the one outcome the temp file was
//! supposed to rule out.
//!
//! The full sequence is:
//!
//! ```text
//!   write the temp file  ->  fsync it     (the bytes are on the platter)
//!   rename over the target                (readers switch atomically)
//!   fsync the directory                   (the rename itself is on the platter)
//! ```
//!
//! Directory fsync is best-effort: some platforms do not permit opening a
//! directory for that purpose, and failing a save because the extra
//! belt-and-braces step was refused would be worse than skipping it.
//!
//! A file holding a secret goes through [`write_atomic_private`] instead,
//! which is the same sequence with the scratch file made owner-only at the
//! moment it is created rather than once the secret is already in it.

use crate::error::{Error, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path`, atomically and durably.
///
/// `tag` distinguishes concurrent writers of the same target: temp names
/// derived only from the target collide, and the loser of the race renames a
/// file that is no longer there.
pub fn write_atomic(path: &Path, bytes: &[u8], tag: &str) -> Result<()> {
    write_atomic_as(path, bytes, tag, false)
}

/// [`write_atomic`] for a file only its owner may read: a bearer token, a
/// private key.
///
/// The scratch file is created `0600` on unix, rather than the finished file
/// being restricted after the rename. Restricting afterwards leaves the
/// secret readable by everyone for as long as the write takes, and another
/// user who opens the scratch file in that window keeps the descriptor --
/// and so the secret -- whatever the mode becomes later. The finished file
/// is the scratch file renamed, so it is born private too.
///
/// A mode the filesystem does not honour is an error, checked before a byte
/// is written: for these files a save that fails is better than a secret
/// that lands where others can read it. Platforms without unix modes get
/// [`write_atomic`]'s behaviour, which is all they had before.
pub fn write_atomic_private(path: &Path, bytes: &[u8], tag: &str) -> Result<()> {
    write_atomic_as(path, bytes, tag, true)
}

fn write_atomic_as(path: &Path, bytes: &[u8], tag: &str, private: bool) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp = dir.join(format!(".{name}.{tag}.tmp"));

    let result = (|| {
        let mut f = if private { open_private(&tmp)? } else { open_scratch(&tmp)? };
        f.write_all(bytes).map_err(|e| Error::io(&tmp, e))?;
        f.sync_all().map_err(|e| Error::io(&tmp, e))?;
        drop(f);
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    })();

    if result.is_err() {
        // Do not leave the scratch file behind to be mistaken for a vault
        // file later, or to collide with the next attempt.
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    sync_dir(dir);
    Ok(())
}

/// The scratch file for an ordinary write, with whatever mode the umask gives.
fn open_scratch(tmp: &Path) -> Result<File> {
    File::create(tmp).map_err(|e| Error::io(tmp, e))
}

/// The scratch file for [`write_atomic_private`], owner-only from creation.
#[cfg(unix)]
fn open_private(tmp: &Path) -> Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // A scratch file left behind by a crashed write is removed rather than
    // reopened: `mode` applies only to a file this call creates, and that
    // one's mode was decided by whoever made it. `create_new` then holds
    // this call to making its own.
    let _ = std::fs::remove_file(tmp);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
        .map_err(|e| Error::io(tmp, e))?;

    // A filesystem mounted without unix permissions accepts the mode and
    // reports whatever its mount options say instead.
    let mode = f.metadata().map_err(|e| Error::io(tmp, e))?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(Error::io(
            tmp,
            std::io::Error::other(format!(
                "could not make this file readable by its owner alone (mode {:o})",
                mode & 0o777
            )),
        ));
    }
    Ok(f)
}

#[cfg(not(unix))]
fn open_private(tmp: &Path) -> Result<File> {
    open_scratch(tmp)
}

/// Create `dir`, and any parents it lacks, with `dir` itself owner-only on
/// unix when this call is what creates it.
///
/// For the directories that hold secrets or a vault: a `0700` directory keeps
/// other users out of everything in it, including files written before
/// anybody thought to restrict them. Only a directory made here is
/// restricted. One that already exists keeps its mode, because somebody
/// chose it -- a vault in a folder deliberately shared with a backup user --
/// and tightening it behind their back would break that without saying so.
/// The parents are made the ordinary way for the same reason: they are not
/// ours.
pub fn create_private_dir(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(Error::io(dir, e)),
    }
}

/// A process-unique tag for [`write_atomic`], distinct per call.
pub fn unique_tag() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Flush a directory entry to disk, so a rename into it survives a crash.
///
/// Best-effort by design -- see the module docs.
pub fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Copy a directory and everything under it.
///
/// A missing source is not an error: a vault with no attachments has no
/// media directory, and that is not a reason to fail a backup.
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
    for entry in std::fs::read_dir(from).map_err(|e| Error::io(from, e))? {
        let entry = entry.map_err(|e| Error::io(from, e))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| Error::io(&src, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thing.json");
        write_atomic(&path, b"first", "t1").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_atomic(&path, b"second", "t2").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn leaves_no_scratch_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_atomic(&dir.path().join("thing.json"), b"x", &unique_tag()).unwrap();
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["thing.json"]);
    }

    #[test]
    fn concurrent_writers_of_one_target_all_succeed() {
        // The regression this exists for: a temp name derived only from the
        // target meant every writer but one renamed a file another had
        // already moved, and failed with `NotFound`.
        let dir = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(dir.path().join("contended"));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || write_atomic(&path, b"same bytes", &unique_tag()))
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("every concurrent write should succeed");
        }
        assert_eq!(std::fs::read(&*path).unwrap(), b"same bytes");
    }

    #[test]
    fn unique_tags_do_not_repeat() {
        let a = unique_tag();
        let b = unique_tag();
        assert_ne!(a, b);
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// The window this exists to close: the scratch file holds the secret
    /// before the rename, so it has to be private before it holds anything.
    #[cfg(unix)]
    #[test]
    fn a_private_scratch_file_is_private_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join(".secret.t.tmp");
        let f = open_private(&tmp).unwrap();
        assert_eq!(std::fs::metadata(&tmp).unwrap().len(), 0);
        assert_eq!(mode_of(&tmp) & 0o077, 0, "the scratch file is readable by somebody else");
        drop(f);
    }

    /// A file an earlier build wrote with the umask's mode -- and restricted
    /// afterwards, or failed to -- is replaced by a private one, not
    /// rewritten in place with the old mode.
    #[cfg(unix)]
    #[test]
    fn a_private_write_replaces_a_readable_file_with_a_private_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_atomic_private(&path, b"token", &unique_tag()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"token");
        assert_eq!(mode_of(&path) & 0o077, 0);
    }

    /// A crash between creating the scratch file and renaming it leaves it
    /// behind, and with a fixed tag `create_new` would otherwise refuse
    /// every save after that one.
    #[test]
    fn a_stale_scratch_file_does_not_block_a_private_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writing.json");
        let stale = dir.path().join(".writing.json.fixed.tmp");
        std::fs::write(&stale, b"half of an older secret").unwrap();

        write_atomic_private(&path, b"whole", "fixed").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"whole");
        assert!(!stale.exists(), "the scratch file was left behind");
        #[cfg(unix)]
        assert_eq!(mode_of(&path) & 0o077, 0);
    }

    #[test]
    fn a_private_directory_can_be_asked_for_more_than_once() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("nested").join("config");
        create_private_dir(&config).unwrap();
        create_private_dir(&config).unwrap();
        assert!(config.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn only_a_directory_this_makes_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let made = dir.path().join("config");
        create_private_dir(&made).unwrap();
        assert_eq!(mode_of(&made), 0o700);

        // Somebody chose this one's mode; asking for it again must not
        // quietly take it away from whoever it was shared with.
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).unwrap();
        create_private_dir(&shared).unwrap();
        assert_eq!(mode_of(&shared), 0o750);
    }
}

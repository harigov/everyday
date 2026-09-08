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
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp = dir.join(format!(".{name}.{tag}.tmp"));

    let result = (|| {
        let mut f = File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
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
}
